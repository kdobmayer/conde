//! The admin API under `/v1/admin`, authenticated by the `CONDE_ADMIN_KEY` bearer: mint, list
//! and revoke host keys, pre-create (or bind) users by Entra identity, disable and enable
//! users. The key is compared through its digest in constant time and never logged; every
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
use super::{BindUser, Clock, ConfigError, Core, CreateUser, Identity, WebStore};
use super::{BridgeRegistry, WebUser};

/// The shortest admin key accepted.
pub(crate) const MIN_KEY_LEN: usize = 32;

struct Inner {
    core: Core,
    key_digest: String,
    /// With Entra configured, pre-created identities must be in its tenant.
    tenant: Option<String>,
}

/// Shared state of the admin routes.
#[derive(Clone)]
pub struct AdminState(Arc<Inner>);

impl AdminState {
    pub(crate) fn new(core: Core, key: &str, tenant: Option<String>) -> AdminState {
        AdminState(Arc::new(Inner {
            core,
            key_digest: digest(key),
            tenant,
        }))
    }

    /// The admin API over `store` with `key` (at least 32 characters) and a given clock.
    pub fn with_clock(
        store: Arc<dyn WebStore>,
        registry: Arc<dyn BridgeRegistry>,
        key: &str,
        tenant: Option<String>,
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
        Ok(AdminState::new(core, key, tenant))
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
        .route("/v1/admin/users", post(create_user))
        .route("/v1/admin/users/{name}/disable", post(disable_user))
        .route("/v1/admin/users/{name}/enable", post(enable_user))
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
    tid: String,
    oid: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
    /// Bind this existing user (or create it under this name) instead of deriving a name.
    #[serde(default)]
    name: Option<String>,
}

fn user_reply(status: StatusCode, user: &WebUser, outcome: &str) -> Response {
    reply(
        status,
        json!({"user": user.name, "status": outcome, "disabled": user.disabled}),
    )
}

/// `POST /v1/admin/users {tid, oid, display, preferred_username?, name?}`: pre-creates the
/// user an Entra identity signs in as, or binds an existing user (`name`) to it once.
async fn create_user(
    State(state): State<AdminState>,
    Json(req): Json<CreateUserRequest>,
) -> Response {
    let valid = |s: &str| {
        (1..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if !valid(&req.tid) || !valid(&req.oid) {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "tid and oid are directory GUIDs",
        );
    }
    if state.0.tenant.as_deref().is_some_and(|t| t != req.tid) {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "tid is not the configured tenant",
        );
    }
    let name = match req.name.as_deref().map(owner_name) {
        Some(None) => {
            return fail(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "name must be [a-z0-9_-], 1 to 40 characters",
            );
        }
        other => other.flatten(),
    };
    let identity = Identity {
        tid: req.tid,
        oid: req.oid,
        display_name: req.display.filter(|d| !d.trim().is_empty()),
        preferred_username: req.preferred_username.filter(|u| !u.trim().is_empty()),
    };
    let result = state
        .core()
        .store(move |s| {
            if let Some(user) = s.user_by_identity(&identity.tid, &identity.oid)? {
                if name.as_ref().is_some_and(|n| *n != user.name) {
                    return Ok(Err(format!("the identity is already bound to '{}'", user.name)));
                }
                return Ok(Ok((user, "exists")));
            }
            if let Some(name) = name {
                return Ok(match s.bind_identity(&name, &identity)? {
                    BindUser::Created(u) => Ok((u, "created")),
                    BindUser::Bound(u) => Ok((u, "bound")),
                    BindUser::NameBound => Err(format!("'{name}' is bound to another identity")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert_eq!(owner_name(" Bob_1 ").as_deref(), Some("bob_1"));
        assert_eq!(owner_name("a/b"), None);
    }
}
