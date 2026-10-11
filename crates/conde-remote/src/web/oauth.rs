//! The remote MCP's OAuth 2.1 authorization server (docs/REMOTE-MCP.md §1-3): the protected
//! resource and authorization server metadata, `/oauth/authorize` (the board's own browser
//! session, then consent) and `/oauth/token` (code with PKCE S256, refresh with rotation).
//! Clients register only through client ID metadata documents ([`super::cimd`]); every client
//! is public. Tokens are opaque and stored as digests: `conde_at_<hex>` for access,
//! `conde_rt_<hex>` for refresh.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use openidconnect::url::{Url, form_urlencoded};
use openidconnect::{PkceCodeChallenge, PkceCodeVerifier};
use serde_json::{Value, json};

use super::cimd::{Client, Refusal};
use super::keys;
use super::ratelimit::ClientIp;
use super::session::{self, WebError, digest, random_token, same_secret};
use super::{McpCode, McpGrant, McpHuman, McpRefreshed, McpTokens, WebState, pages};

/// The one scope: the board as the signed-in user.
pub(crate) const SCOPE: &str = "conde";
pub(crate) const ACCESS_PREFIX: &str = "conde_at_";
const REFRESH_PREFIX: &str = "conde_rt_";

/// The query or form parameters; a repeated parameter is refused (RFC 6749 §3.1).
fn params(raw: &[u8]) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    for (k, v) in form_urlencoded::parse(raw) {
        if out.insert(k.into_owned(), v.into_owned()).is_some() {
            return None;
        }
    }
    Some(out)
}

fn get<'a>(p: &'a HashMap<String, String>, key: &str) -> &'a str {
    p.get(key).map(String::as_str).unwrap_or("")
}

/// Two resource URIs name the same resource (scheme and host compared case-insensitively).
fn same_resource(a: &str, b: &str) -> bool {
    matches!((Url::parse(a), Url::parse(b)), (Ok(a), Ok(b)) if a == b)
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn metadata(body: Value) -> Response {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("max-age=300"))],
        Json(body),
    )
        .into_response()
}

/// `GET /.well-known/oauth-protected-resource[/mcp]` (RFC 9728).
pub(crate) async fn protected_resource(State(state): State<WebState>) -> Response {
    let c = state.config();
    metadata(json!({
        "resource": c.mcp_resource(),
        "authorization_servers": [c.mcp_issuer()],
        "scopes_supported": [SCOPE],
        "bearer_methods_supported": ["header"],
    }))
}

/// `GET /.well-known/oauth-authorization-server` (RFC 8414): no registration endpoint, public
/// clients identified by their metadata document, PKCE S256 only.
pub(crate) async fn authorization_server(State(state): State<WebState>) -> Response {
    let issuer = state.config().mcp_issuer();
    metadata(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "scopes_supported": [SCOPE],
        "authorization_response_iss_parameter_supported": true,
    }))
}

/// An authorization request that passed every check.
struct Vetted {
    client: Client,
    redirect: Url,
    challenge: String,
    state: String,
    resource: String,
}

/// A refused authorization request.
enum Bounce {
    /// Before the redirect URI is trusted: a page, never a redirect.
    Page(Refusal),
    /// After it is: back to the client with an OAuth error.
    Back {
        redirect: Url,
        state: Option<String>,
        error: &'static str,
        description: &'static str,
    },
}

fn pkce_safe(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// The checks of an authorization request, repeated on consent (nothing pending is stored).
async fn vet(state: &WebState, p: &HashMap<String, String>) -> Result<Vetted, Bounce> {
    let clients = &state.0.clients;
    let client = clients.client(get(p, "client_id")).await.map_err(Bounce::Page)?;
    let redirect = clients.check_redirect(&client, get(p, "redirect_uri")).map_err(Bounce::Page)?;
    let st = p.get("state").filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control));
    let back = |error, description| Bounce::Back {
        redirect: redirect.clone(),
        state: st.cloned(),
        error,
        description,
    };
    let Some(st) = st else {
        return Err(back("invalid_request", "state is required"));
    };
    if get(p, "response_type") != "code" {
        return Err(back("unsupported_response_type", "only response_type=code is supported"));
    }
    if get(p, "code_challenge_method") != "S256" {
        return Err(back("invalid_request", "PKCE with code_challenge_method=S256 is required"));
    }
    let challenge = get(p, "code_challenge");
    if challenge.len() != 43 || !pkce_safe(challenge) {
        return Err(back("invalid_request", "code_challenge must be a base64url SHA-256 (43 characters)"));
    }
    let ours = state.config().mcp_resource();
    if p.get("resource").is_some_and(|r| !same_resource(r, &ours)) {
        return Err(back("invalid_target", "this server is the resource named in its metadata only"));
    }
    Ok(Vetted {
        client,
        redirect,
        challenge: challenge.to_string(),
        state: st.clone(),
        resource: ours,
    })
}

/// A redirect to the client with `pairs` and the issuer (RFC 9207).
fn back(state: &WebState, redirect: &Url, pairs: &[(&str, &str)]) -> Response {
    let mut url = redirect.clone();
    {
        let mut q = url.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
        q.append_pair("iss", &state.config().mcp_issuer());
    }
    Redirect::to(url.as_str()).into_response()
}

fn bounce(state: &WebState, b: Bounce) -> Response {
    match b {
        Bounce::Page(r) => {
            tracing::warn!(reason = r.code(), "mcp authorization refused");
            let message = format!("{} (invalid_client: {})", r.message(), r.code());
            pages::error(StatusCode::BAD_REQUEST, "Connection refused", &message)
        }
        Bounce::Back { redirect, state: st, error, description } => {
            tracing::warn!(error, description, "mcp authorization request refused");
            let mut pairs = vec![("error", error), ("error_description", description)];
            if let Some(st) = st.as_deref() {
                pairs.push(("state", st));
            }
            back(state, &redirect, &pairs)
        }
    }
}

fn too_many_page(ip: std::net::IpAddr, wait: u64) -> Response {
    tracing::info!(client_ip = %ip, wait, endpoint = "authorize", "oauth rate limited");
    let mut r = pages::error(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many requests",
        &format!("Too many sign-in requests from this address. Retry in {wait} s."),
    );
    if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// The parameters a consent form carries back.
const ECHOED: &[&str] = &[
    "response_type",
    "client_id",
    "redirect_uri",
    "code_challenge",
    "code_challenge_method",
    "state",
    "resource",
    "scope",
];

/// `GET /oauth/authorize`: checks the request, signs the browser in if needed, then asks.
pub(crate) async fn authorize(
    State(state): State<WebState>,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    uri: Uri,
) -> Response {
    if let Err(wait) = state.0.limits.oauth(ip) {
        return too_many_page(ip, wait);
    }
    let Some(p) = params(uri.query().unwrap_or("").as_bytes()) else {
        return bounce(&state, Bounce::Page(Refusal::DuplicateParameter));
    };
    let v = match vet(&state, &p).await {
        Ok(v) => v,
        Err(b) => return bounce(&state, b),
    };
    let session = match session::current(&state, &jar).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            let here = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/oauth/authorize");
            return keys::to_login(here);
        }
        Err(e) => return e.into_response(),
    };
    let user_id = session.record.user_id;
    let human = match state.store(move |s| s.mcp_human(user_id)).await {
        Ok(h) => h,
        Err(e) => return WebError::Store(e).into_response(),
    };
    let fields: Vec<(&'static str, String)> =
        ECHOED.iter().filter_map(|k| p.get(*k).map(|v| (*k, v.clone()))).collect();
    let acts_as = match &human {
        McpHuman::One(name) => Ok(name.as_str()),
        McpHuman::Refused(why) => Err(why.as_str()),
    };
    let redirect_host = v.redirect.host_str().unwrap_or_default().to_string();
    let consent = pages::Consent {
        client_host: &v.client.host,
        client_name: &v.client.name,
        redirect_host: &redirect_host,
        acts_as,
        fields: &fields,
    };
    pages::consent(keys::header(&session), &consent, &origin(&v.redirect), &keys::idp_origin(&state))
}

/// `POST /oauth/authorize`: the consent answer (session and CSRF), every check repeated. Allow
/// returns a one-time code bound to the user, client, redirect URI and challenge.
pub(crate) async fn consent(
    State(state): State<WebState>,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    body: Bytes,
) -> Response {
    if let Err(wait) = state.0.limits.oauth(ip) {
        return too_many_page(ip, wait);
    }
    let Some(p) = params(&body) else {
        return bounce(&state, Bounce::Page(Refusal::DuplicateParameter));
    };
    let session = match session::current(&state, &jar).await {
        Ok(Some(s)) => s,
        Ok(None) => return keys::signed_out().into_response(),
        Err(e) => return e.into_response(),
    };
    if let Err(e) = session.check_csrf(get(&p, "csrf")) {
        return e.into_response();
    }
    let v = match vet(&state, &p).await {
        Ok(v) => v,
        Err(b) => return bounce(&state, b),
    };
    let user = session.record.user_name.clone();
    match get(&p, "decision") {
        "allow" => {}
        "deny" => {
            tracing::info!(%user, client = %v.client.host, decision = "deny", "mcp consent");
            return back(&state, &v.redirect, &[("error", "access_denied"), ("state", &v.state)]);
        }
        _ => {
            return back(
                &state,
                &v.redirect,
                &[("error", "invalid_request"), ("error_description", "no decision"), ("state", &v.state)],
            );
        }
    }
    let code = random_token();
    let now = state.now();
    let record = McpCode {
        user_id: session.record.user_id,
        client_id: v.client.client_id.clone(),
        client_name: Some(v.client.name.clone()),
        redirect_uri: get(&p, "redirect_uri").to_string(),
        challenge: v.challenge.clone(),
        resource: v.resource.clone(),
        created_at: now,
    };
    let (key, ttl) = (digest(&code), state.config().mcp.code_ttl.as_secs() as i64);
    if let Err(e) = state
        .store(move |s| {
            s.purge_mcp(now, ttl)?;
            s.insert_mcp_code(&key, &record)
        })
        .await
    {
        return WebError::Store(e).into_response();
    }
    tracing::info!(%user, client = %v.client.host, client_id = %v.client.client_id, decision = "allow", "mcp consent");
    back(&state, &v.redirect, &[("code", &code), ("state", &v.state)])
}

fn token_response(status: StatusCode, body: Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    r
}

fn token_error(error: &str, description: &str) -> Response {
    let status = if error == "invalid_client" { StatusCode::UNAUTHORIZED } else { StatusCode::BAD_REQUEST };
    token_response(status, json!({"error": error, "error_description": description}))
}

fn server_error(e: super::WebStoreError) -> Response {
    tracing::error!(error = %e, "mcp token store failed");
    token_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"error": "server_error", "error_description": "the store did not answer; retry"}),
    )
}

/// A fresh access and refresh token, with their digests at the configured lifetimes.
fn new_tokens(state: &WebState) -> (String, String, McpTokens) {
    let access = format!("{ACCESS_PREFIX}{}", random_token());
    let refresh = format!("{REFRESH_PREFIX}{}", random_token());
    let m = &state.config().mcp;
    let tokens = McpTokens {
        access_digest: digest(&access),
        refresh_digest: digest(&refresh),
        access_ttl: m.access_ttl.as_secs() as i64,
        refresh_ttl: m.refresh_ttl.as_secs() as i64,
    };
    (access, refresh, tokens)
}

fn issued(access: &str, refresh: &str, expires_in: i64) -> Response {
    token_response(
        StatusCode::OK,
        json!({"access_token": access, "token_type": "Bearer", "expires_in": expires_in.max(0),
               "refresh_token": refresh, "scope": SCOPE}),
    )
}

/// `POST /oauth/token` (form): `authorization_code` creates the grant, `refresh_token` rotates.
pub(crate) async fn token(State(state): State<WebState>, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    if let Err(wait) = state.0.limits.oauth(ip) {
        tracing::info!(client_ip = %ip, wait, endpoint = "token", "oauth rate limited");
        let mut r = token_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "rate_limited", "error_description": format!("retry in {wait} s")}),
        );
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return r;
    }
    let Some(p) = params(&body) else {
        return token_error("invalid_request", "a parameter is repeated");
    };
    if let Some(r) = p.get("resource")
        && !same_resource(r, &state.config().mcp_resource())
    {
        return token_error("invalid_target", "this server is the resource named in its metadata only");
    }
    match get(&p, "grant_type") {
        "authorization_code" => code_grant(&state, &p).await,
        "refresh_token" => refresh_grant(&state, &p).await,
        "" => token_error("invalid_request", "grant_type is required"),
        _ => token_error("unsupported_grant_type", "authorization_code and refresh_token only"),
    }
}

async fn code_grant(state: &WebState, p: &HashMap<String, String>) -> Response {
    let code = get(p, "code");
    if code.len() != 64 {
        return token_error("invalid_grant", "unknown code");
    }
    let key = digest(code);
    let taken = match state.store(move |s| s.take_mcp_code(&key)).await {
        Ok(t) => t,
        Err(e) => return server_error(e),
    };
    let Some(c) = taken else {
        tracing::warn!("mcp token: unknown or used code");
        return token_error("invalid_grant", "unknown or already used code");
    };
    let now = state.now();
    let m = &state.config().mcp;
    if now - c.created_at > m.code_ttl.as_secs() as i64 {
        return token_error("invalid_grant", "the code expired");
    }
    if get(p, "client_id") != c.client_id {
        return token_error("invalid_grant", "the code was issued to another client");
    }
    if get(p, "redirect_uri") != c.redirect_uri {
        return token_error("invalid_grant", "redirect_uri differs from the authorization request");
    }
    let verifier = get(p, "code_verifier");
    if !pkce_safe(verifier) {
        return token_error("invalid_grant", "bad code_verifier");
    }
    let expected = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier.to_string()));
    if !same_secret(expected.as_str(), &c.challenge) {
        return token_error("invalid_grant", "the code_verifier does not match the challenge");
    }
    let client_host = Url::parse(&c.client_id).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let grant = McpGrant {
        grant_id: random_token()[..32].to_string(),
        user_id: c.user_id,
        client_id: c.client_id,
        client_host,
        client_name: c.client_name,
        resource: c.resource,
        created_at: now,
        last_used_at: None,
        expires_at: now + m.grant_ttl.as_secs() as i64,
        revoked_at: None,
    };
    let (access, refresh, tokens) = new_tokens(state);
    let stored = grant.clone();
    match state.store(move |s| s.create_mcp_grant(&stored, &tokens, now)).await {
        Ok(Some(expires_at)) => {
            tracing::info!(user_id = grant.user_id, grant = %grant.grant_id, client = %grant.client_host,
                client_id = %grant.client_id, "mcp grant created");
            issued(&access, &refresh, expires_at - now)
        }
        Ok(None) => token_error("invalid_grant", "this account is disabled"),
        Err(e) => server_error(e),
    }
}

async fn refresh_grant(state: &WebState, p: &HashMap<String, String>) -> Response {
    let presented = get(p, "refresh_token");
    if !presented.starts_with(REFRESH_PREFIX) || presented.len() != REFRESH_PREFIX.len() + 64 {
        return token_error("invalid_grant", "unknown refresh token");
    }
    let client_id = get(p, "client_id").to_string();
    if client_id.is_empty() {
        return token_error("invalid_request", "client_id is required");
    }
    let (access, refresh, tokens) = new_tokens(state);
    let (key, now, ttl) = (digest(presented), state.now(), state.config().mcp.code_ttl.as_secs() as i64);
    let refreshed = state.store(move |s| {
        let refreshed = s.refresh_mcp_grant(&key, &client_id, &tokens, now)?;
        // After the rotation, which is committed: a failed purge must not cost the new tokens.
        if let Err(e) = s.purge_mcp(now, ttl) {
            tracing::warn!("mcp purge on refresh failed: {e}");
        }
        Ok(refreshed)
    });
    match refreshed.await {
        Ok(McpRefreshed::Rotated { grant, access_expires_at }) => {
            tracing::info!(user_id = grant.user_id, grant = %grant.grant_id, client = %grant.client_host, "mcp token refreshed");
            issued(&access, &refresh, access_expires_at - now)
        }
        Ok(McpRefreshed::Reused { grant }) => {
            tracing::warn!(user_id = grant.user_id, grant = %grant.grant_id, client = %grant.client_host,
                by = "refresh token reuse", "mcp refresh token presented again; grant revoked");
            token_error("invalid_grant", "this refresh token was already used; the connection was revoked")
        }
        Ok(McpRefreshed::Invalid) => {
            tracing::warn!("mcp token: refresh refused");
            token_error("invalid_grant", "unknown, expired or revoked refresh token")
        }
        Err(e) => server_error(e),
    }
}

/// Deletes expired codes, tokens and grants every `sweep_every` until shutdown (each replica
/// sweeps; the deletes are idempotent).
pub(crate) async fn sweep(state: WebState, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let every = state.config().mcp.sweep_every;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = crate::protocol::shutdown_signal(&mut shutdown) => return,
            _ = tick.tick() => {}
        }
        let (now, ttl) = (state.now(), state.config().mcp.code_ttl.as_secs() as i64);
        if let Err(e) = state.store(move |s| s.purge_mcp(now, ttl)).await {
            tracing::warn!("mcp sweep failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_parameters_are_refused() {
        assert!(params(b"a=1&b=2").is_some());
        assert!(params(b"a=1&a=2").is_none());
        assert_eq!(get(&params(b"x=%2F").unwrap(), "x"), "/");
    }

    #[test]
    fn resources_compare_after_normalising_scheme_and_host() {
        assert!(same_resource("https://Conde.Example/mcp", "https://conde.example/mcp"));
        assert!(!same_resource("https://conde.example/mcp/", "https://conde.example/mcp"));
        assert!(!same_resource("https://conde.example", "https://conde.example/mcp"));
        assert!(!same_resource("not a url", "https://conde.example/mcp"));
    }
}
