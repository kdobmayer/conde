//! The admin API under `/v1/admin`, authenticated by the `CONDE_ADMIN_KEY` bearer: mint, list
//! and revoke host keys, invite users by email, bind users to an identity, list, disable and
//! enable users. The key is compared through its digest in constant time and never logged; every
//! answer is `no-store`.

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::auth::base_name;
use super::keys::{MintError, host_slug, mint};
use super::session::digest;
use super::{BindUser, Clock, ConfigError, Core, CreateUser, Identity, Invited, WebConfig, WebStore};
use super::{BridgeRegistry, WebUser, entra_issuer};

/// The shortest admin key accepted.
pub(crate) const MIN_KEY_LEN: usize = 32;

/// The sign-in issuer bindings are checked against.
struct Issuer {
    url: String,
    entra_tenant: Option<String>,
}

struct Inner {
    core: Core,
    key_digest: String,
    /// With sign-in configured, bound identities must be of its issuer.
    issuer: Option<Issuer>,
}

/// Shared state of the admin routes.
#[derive(Clone)]
pub struct AdminState(Arc<Inner>);

impl AdminState {
    pub(crate) fn new(core: Core, key: &str, sign_in: Option<&WebConfig>) -> AdminState {
        AdminState(Arc::new(Inner {
            core,
            key_digest: digest(key),
            issuer: sign_in.map(|c| Issuer {
                url: c.issuer.clone(),
                entra_tenant: c.entra_tenant.clone(),
            }),
        }))
    }

    /// The admin API over `store` with `key` (at least 32 characters), the sign-in it binds
    /// identities for, and a given clock.
    pub fn with_clock(
        store: Arc<dyn WebStore>,
        registry: Arc<dyn BridgeRegistry>,
        key: &str,
        sign_in: Option<&WebConfig>,
        clock: Clock,
    ) -> Result<AdminState, ConfigError> {
        if key.len() < MIN_KEY_LEN {
            return Err(ConfigError::AdminKey(MIN_KEY_LEN));
        }
        let core = Core {
            store,
            registry,
            clock,
        };
        Ok(AdminState::new(core, key, sign_in))
    }

    fn core(&self) -> &Core {
        &self.0.core
    }
}

/// Equality that takes the same time wherever the inputs differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(state: &AdminState, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer")
        && constant_time_eq(
            digest(token.trim()).as_bytes(),
            state.0.key_digest.as_bytes(),
        )
}

async fn require_admin(State(state): State<AdminState>, request: Request, next: Next) -> Response {
    if !authorized(&state, request.headers()) {
        tracing::warn!(path = %request.uri().path(), "admin request without a valid key");
        let mut r = fail(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "admin key required",
        );
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return r;
    }
    next.run(request).await
}

fn reply(status: StatusCode, body: Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    reply(status, json!({"error": code, "message": message}))
}

fn store_failed(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "admin store call failed");
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "store failed",
    )
}

/// An owner name: `[a-z0-9_-]`, 1 to 40 characters.
fn owner_name(input: &str) -> Option<String> {
    let name = input.trim().to_lowercase();
    let ok = (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(name)
}

/// The admin routes; every one requires the admin bearer key.
pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/v1/admin/keys", get(list_keys).post(create_key))
        .route("/v1/admin/keys/{id}/revoke", post(revoke_key))
        .route("/v1/admin/users", get(list_users).post(create_user))
        .route("/v1/admin/users/{name}/disable", post(disable_user))
        .route("/v1/admin/users/{name}/enable", post(enable_user))
        .route("/v1/admin/users/{name}/slack", post(set_slack))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin))
        .with_state(state)
}

#[derive(Deserialize)]
struct CreateKey {
    user: String,
    host_label: String,
    /// The host id, when it should differ from the label's slug.
    #[serde(default)]
    host: Option<String>,
}

/// `POST /v1/admin/keys {user, host_label, host?}`: the key, shown once.
async fn create_key(State(state): State<AdminState>, Json(req): Json<CreateKey>) -> Response {
    let Some(user) = owner_name(&req.user) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "user must be [a-z0-9_-], 1 to 40 characters",
        );
    };
    let label = req.host_label.trim().to_string();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "host_label must be 1 to 64 characters",
        );
    }
    let Some(base) = host_slug(req.host.as_deref().unwrap_or(&label)) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "no usable host id in host or host_label",
        );
    };
    let name = user.clone();
    let owner = match state.core().store(move |s| s.ensure_user(&name)).await {
        Ok(u) => u,
        Err(e) => return store_failed(e),
    };
    match mint(state.core(), owner.user_id, base.clone(), label.clone(), 1).await {
        Ok(k) => {
            tracing::info!(%user, host = %k.host_id, "admin minted a host key");
            reply(
                StatusCode::OK,
                json!({"key_id": &digest(&k.key)[..12], "user": user, "host_id": k.host_id,
                       "label": label, "key": k.key, "replaced": k.replaced}),
            )
        }
        Err(MintError::HostTaken) => fail(
            StatusCode::CONFLICT,
            "host_taken",
            &format!("host '{base}' belongs to another user"),
        ),
        Err(MintError::Disabled) => fail(StatusCode::FORBIDDEN, "disabled", "the user is disabled"),
        Err(MintError::Store(e)) => store_failed(e),
    }
}

/// `GET /v1/admin/keys`: every credential (never a key).
async fn list_keys(State(state): State<AdminState>) -> Response {
    match state.core().store(|s| s.all_keys()).await {
        Ok(keys) => reply(StatusCode::OK, json!({"keys": keys})),
        Err(e) => store_failed(e),
    }
}

/// `POST /v1/admin/keys/{id}/revoke`: revokes one live key (its 12-character id) and closes
/// its host's live bridge connection.
async fn revoke_key(State(state): State<AdminState>, Path(id): Path<String>) -> Response {
    let id = id.to_lowercase();
    if id.len() != 12 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "a key id is 12 hex characters",
        );
    }
    let now = state.core().now();
    let key_id = id.clone();
    match state
        .core()
        .store(move |s| s.revoke_key(&key_id, now))
        .await
    {
        Ok(Some(host)) => {
            state.core().registry.disconnect_host(&host);
            tracing::info!(key_id = %id, %host, "admin revoked a host key");
            reply(StatusCode::OK, json!({"key_id": id, "host_id": host}))
        }
        Ok(None) => fail(
            StatusCode::NOT_FOUND,
            "not_found",
            "no live key has this id",
        ),
        Err(e) => store_failed(e),
    }
}

#[derive(Deserialize)]
struct CreateUserRequest {
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    /// The Entra form of `issuer` (the tenant), for older callers.
    #[serde(default)]
    tid: Option<String>,
    /// The Entra form of `subject` (the object id), for older callers.
    #[serde(default)]
    oid: Option<String>,
    /// The invite's email.
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
    /// The invited user, or the existing user to bind (or create under this name) instead of
    /// deriving a name.
    #[serde(default)]
    name: Option<String>,
}

fn user_reply(status: StatusCode, user: &WebUser, outcome: &str) -> Response {
    reply(
        status,
        json!({"user": user.name, "status": outcome, "disabled": user.disabled}),
    )
}

fn bad_request(message: &str) -> Response {
    fail(StatusCode::BAD_REQUEST, "bad_request", message)
}

/// An email address as an invite takes it: trimmed, one `@` between a local part and a domain,
/// no spaces or control characters, at most 254 characters.
pub(crate) fn email_address(input: &str) -> Option<String> {
    let email = input.trim();
    let ok = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty() && !domain.contains('@'));
    ok.then(|| email.to_string())
}

/// The issuer a binding request names: `{issuer, subject}`, or Entra's `{tid, oid}` mapped to
/// the tenant's issuer; with sign-in configured, it must be the configured issuer.
fn binding_key(state: &AdminState, req: &CreateUserRequest) -> Result<(String, String), String> {
    let configured = state.0.issuer.as_ref();
    match (&req.issuer, &req.subject, &req.tid, &req.oid) {
        (Some(issuer), Some(subject), None, None) => {
            let issuer = issuer.trim();
            let url_ok = (issuer.starts_with("https://") || issuer.starts_with("http://"))
                && issuer.len() <= 512
                && !issuer.chars().any(|c| c.is_whitespace() || c.is_control());
            if !url_ok {
                return Err("issuer is the provider's issuer URL".into());
            }
            if configured.is_some_and(|c| c.url != issuer) {
                return Err("issuer is not the configured issuer".into());
            }
            if configured.is_none() && !issuer.starts_with("https://") {
                return Err("issuer is the provider's https issuer URL".into());
            }
            if subject.is_empty() || subject.len() > 255 || subject.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err("subject is the identity's sub claim, 1 to 255 characters".into());
            }
            Ok((issuer.to_string(), subject.clone()))
        }
        (None, None, Some(tid), Some(oid)) => {
            let valid = |s: &str| {
                (1..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            };
            if !valid(tid) || !valid(oid) {
                return Err("tid and oid are directory GUIDs".into());
            }
            match configured {
                None => Ok((entra_issuer(tid), oid.clone())),
                Some(Issuer { url, entra_tenant: Some(tenant) }) if tenant == tid => Ok((url.clone(), oid.clone())),
                Some(Issuer { entra_tenant: Some(_), .. }) => Err("tid is not the configured tenant".into()),
                Some(Issuer { url, entra_tenant: None }) => Err(format!("this board signs in with {url}, not Entra; pass issuer and subject")),
            }
        }
        _ => Err(
            "pass {name, email} to invite a user, or {issuer, subject} (or Entra's {tid, oid}) to bind an identity".into(),
        ),
    }
}

/// `POST /v1/admin/users`: `{name, email}` invites a user (created with the email the first
/// sign-in must verify); `{issuer, subject, display?, preferred_username?, name?}` (or Entra's
/// `{tid, oid, ...}`) pre-creates the user an identity signs in as, or binds an existing user
/// (`name`) to it once per issuer.
async fn create_user(
    State(state): State<AdminState>,
    Json(req): Json<CreateUserRequest>,
) -> Response {
    let binding = [&req.issuer, &req.subject, &req.tid, &req.oid].iter().any(|v| v.is_some());
    if !binding {
        return invite_user(state, req).await;
    }
    if req.email.is_some() {
        return bad_request("email goes with the invite, {name, email}, not with an identity");
    }
    let (issuer, subject) = match binding_key(&state, &req) {
        Ok(key) => key,
        Err(message) => return bad_request(&message),
    };
    let name = match req.name.as_deref().map(owner_name) {
        Some(None) => return bad_request("name must be [a-z0-9_-], 1 to 40 characters"),
        other => other.flatten(),
    };
    let preferred_username = req.preferred_username.filter(|u| !u.trim().is_empty());
    let identity = Identity {
        issuer,
        subject,
        display_name: req.display.filter(|d| !d.trim().is_empty()),
        email: super::auth::lookup_email(preferred_username.as_deref(), None),
        preferred_username,
        verified_email: None,
    };
    let result = state
        .core()
        .store(move |s| {
            if let Some(user) = s.user_by_identity(&identity.issuer, &identity.subject)? {
                if name.as_ref().is_some_and(|n| *n != user.name) {
                    return Ok(Err(format!("the identity is already bound to '{}'", user.name)));
                }
                return Ok(Ok((user, "exists")));
            }
            if let Some(name) = name {
                return Ok(match s.bind_identity(&name, &identity)? {
                    BindUser::Created(u) => Ok((u, "created")),
                    BindUser::Bound(u) => Ok((u, "bound")),
                    BindUser::NameBound => Err(format!("'{name}' is bound to another identity of this issuer")),
                });
            }
            let base = base_name(&identity);
            for n in 1..=9 {
                let candidate = if n == 1 { base.clone() } else { format!("{base}-{n}") };
                match s.create_user(&identity, &candidate)? {
                    CreateUser::Created(u) => return Ok(Ok((u, "created"))),
                    CreateUser::NameTaken { bound: false } if n == 1 => {
                        return Ok(Err(format!(
                            "a user named '{candidate}' exists without an identity; pass \"name\" to bind it"
                        )));
                    }
                    CreateUser::NameTaken { .. } => continue,
                }
            }
            Ok(Err(format!("no free user name derived from '{base}'; pass \"name\"")))
        })
        .await;
    match result {
        Ok(Ok((user, outcome))) => {
            tracing::info!(user = %user.name, outcome, "admin user");
            user_reply(StatusCode::OK, &user, outcome)
        }
        Ok(Err(conflict)) => fail(StatusCode::CONFLICT, "conflict", &conflict),
        Err(e) => store_failed(e),
    }
}

/// The invite form of `POST /v1/admin/users`: `{name, email}`.
async fn invite_user(state: AdminState, req: CreateUserRequest) -> Response {
    let (Some(name), Some(email)) = (req.name.as_deref(), req.email.as_deref()) else {
        return bad_request("an invite is {name, email}");
    };
    let Some(name) = owner_name(name) else {
        return bad_request("name must be [a-z0-9_-], 1 to 40 characters");
    };
    let Some(email) = email_address(email) else {
        return bad_request("email must be an address like name@example.com");
    };
    let (target, address) = (name.clone(), email.clone());
    let result = state.core().store(move |s| s.invite_user(&target, &address)).await;
    let (user, outcome) = match result {
        Ok(Invited::Created(u)) => (u, "invited"),
        Ok(Invited::EmailSet(u)) => (u, "email set"),
        Ok(Invited::Exists(u)) => (u, "exists"),
        Ok(Invited::OtherEmail(other)) => {
            return fail(StatusCode::CONFLICT, "conflict", &format!("'{name}' exists with another email ({other})"));
        }
        Ok(Invited::EmailTaken(other)) => {
            return fail(StatusCode::CONFLICT, "conflict", &format!("user '{other}' has this email already"));
        }
        Err(e) => return store_failed(e),
    };
    tracing::info!(user = %user.name, outcome, "admin invited a user");
    reply(
        StatusCode::OK,
        json!({"user": user.name, "email": email, "status": outcome, "disabled": user.disabled}),
    )
}

/// `GET /v1/admin/users`: every user with their email, the issuers they are bound to, and
/// whether they are disabled.
async fn list_users(State(state): State<AdminState>) -> Response {
    match state.core().store(|s| s.list_users()).await {
        Ok(users) => reply(StatusCode::OK, json!({"users": users})),
        Err(e) => store_failed(e),
    }
}

async fn set_disabled(state: AdminState, name: String, disabled: bool) -> Response {
    let Some(name) = owner_name(&name) else {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "bad user name");
    };
    let target = name.clone();
    match state
        .core()
        .store(move |s| s.set_disabled(&target, disabled))
        .await
    {
        Ok(Some(hosts)) => {
            if disabled {
                for host in &hosts {
                    state.core().registry.disconnect_host(host);
                }
            }
            tracing::info!(user = %name, disabled, "admin set the user's disabled flag");
            let disconnected = if disabled { hosts } else { Vec::new() };
            reply(
                StatusCode::OK,
                json!({"user": name, "disabled": disabled, "disconnected": disconnected}),
            )
        }
        Ok(None) => fail(StatusCode::NOT_FOUND, "not_found", "no such user"),
        Err(e) => store_failed(e),
    }
}

/// `POST /v1/admin/users/{name}/disable`: signs the user out of the board and closes their
/// hosts' live bridge connections; their keys stop authenticating.
async fn disable_user(State(state): State<AdminState>, Path(name): Path<String>) -> Response {
    set_disabled(state, name, true).await
}

/// `POST /v1/admin/users/{name}/enable`.
async fn enable_user(State(state): State<AdminState>, Path(name): Path<String>) -> Response {
    set_disabled(state, name, false).await
}

#[derive(Deserialize)]
struct SlackMember {
    member_id: String,
}

/// A Slack member id as Slack writes one: `U...` or `W...`, upper-case letters and digits.
fn member_id(input: &str) -> Option<String> {
    let id = input.trim();
    let ok = (3..=32).contains(&id.len())
        && id.starts_with(['U', 'W'])
        && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    ok.then(|| id.to_string())
}

/// `POST /v1/admin/users/{name}/slack {member_id}`: the Slack member id pages to the user's human
/// reach; set by the admin, it is never replaced by a lookup by email.
async fn set_slack(State(state): State<AdminState>, Path(name): Path<String>, Json(req): Json<SlackMember>) -> Response {
    let Some(name) = owner_name(&name) else {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "bad user name");
    };
    let Some(id) = member_id(&req.member_id) else {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "member_id is a Slack member id (U... or W...)");
    };
    let (target, member) = (name.clone(), id.clone());
    match state.core().store(move |s| s.set_slack_member(&target, &member)).await {
        Ok(Some(user)) => {
            tracing::info!(user = %user.name, "admin set the user's Slack member id");
            reply(StatusCode::OK, json!({"user": user.name, "slack_member_id": id, "slack_source": "admin"}))
        }
        Ok(None) => fail(StatusCode::NOT_FOUND, "not_found", "no such user"),
        Err(e) => store_failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_member_ids() {
        assert_eq!(member_id(" U0123ABC ").as_deref(), Some("U0123ABC"));
        assert_eq!(member_id("W99").as_deref(), Some("W99"));
        for bad in ["", "U1", "u0123", "C0123ABC", "U01 23", "U0123abc"] {
            assert_eq!(member_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn invite_emails() {
        assert_eq!(email_address(" Ann.B@Example.com ").as_deref(), Some("Ann.B@Example.com"));
        for bad in ["", "ann", "@example.com", "ann@", "a@b@c", "ann b@example.com", "ann\u{7}@x.io"] {
            assert_eq!(email_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn constant_time_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert_eq!(owner_name(" Bob_1 ").as_deref(), Some("bob_1"));
        assert_eq!(owner_name("a/b"), None);
    }
}
