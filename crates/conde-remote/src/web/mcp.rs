//! `POST /mcp`: the remote MCP endpoint (docs/REMOTE-MCP.md §1, §2, §5), stateless and JSON
//! only. Every request carries a bearer token of a live grant whose audience is this endpoint;
//! cookies are never read. The era is decided per request: per-request `_meta` (2026-07-28,
//! with its mirrored headers checked), else `initialize`, else the `MCP-Protocol-Version` header
//! (absent: 2025-03-26). No sessions, no GET stream. The tools run in the [`Dispatcher::mcp`]
//! hook, as the grant's user.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use serde_json::{Value, json};

use super::WebState;
use super::oauth::{ACCESS_PREFIX, SCOPE};
use super::session::digest;
use crate::mcp_protocol::{
    HEADER_MISMATCH, HEADERLESS_VERSION, INTERNAL_ERROR, INVALID_REQUEST, LEGACY_VERSIONS, META_VERSION,
    METHOD_NOT_FOUND, MODERN_VERSIONS, PARSE_ERROR, UNSUPPORTED_PROTOCOL_VERSION, error_response, supported_versions,
};
use crate::{Dispatcher, McpCaller};

/// The largest request body.
pub const MAX_BODY: usize = 512 * 1024;
/// The tools that write as the user's human name: they also draw on the user's write bucket.
const WRITE_TOOLS: &[&str] = &["send", "react"];

#[derive(Clone)]
pub(crate) struct McpState {
    pub web: WebState,
    pub dispatcher: Arc<dyn Dispatcher>,
}

pub(crate) fn routes(state: McpState) -> Router {
    Router::new()
        .route("/mcp", post(endpoint))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

fn json_response(status: StatusCode, body: Value) -> Response {
    let mut r = (status, axum::Json(body)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn rpc_error(status: StatusCode, id: &Value, code: i64, message: &str) -> Response {
    json_response(status, error_response(id, code, message, None))
}

/// 401 with the challenge that points the client at the resource metadata.
fn challenge(web: &WebState, token_sent: bool) -> Response {
    let mut value = format!("Bearer resource_metadata=\"{}\", scope=\"{SCOPE}\"", web.config().mcp_resource_metadata());
    if token_sent {
        value.push_str(", error=\"invalid_token\"");
    }
    let mut r = (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response();
    if let Ok(v) = HeaderValue::from_str(&value) {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// An Origin a browser-borne request may come from: this server, or an allowed client's site.
fn allowed_origin(web: &WebState, origin: &str) -> bool {
    origin.eq_ignore_ascii_case(&web.config().public_url)
        || web.0.clients.hosts().iter().any(|h| origin.eq_ignore_ascii_case(&format!("https://{h}")))
}

/// The authenticated caller of a request: a live grant of an enabled user, the token unexpired
/// and issued for this endpoint; else the response refusing it (boxed: a `Response` is large).
async fn gate(web: &WebState, headers: &HeaderMap) -> Result<McpCaller, Box<Response>> {
    let refuse = |reason: &str, grant: &str| {
        tracing::warn!(reason, grant, "mcp request unauthenticated");
    };
    let Some(token) = bearer(headers) else {
        refuse("no token", "");
        return Err(Box::new(challenge(web, false)));
    };
    if !token.starts_with(ACCESS_PREFIX) || token.len() != ACCESS_PREFIX.len() + 64 {
        refuse("not an access token", "");
        return Err(Box::new(challenge(web, true)));
    }
    let (key, now) = (digest(token), web.now());
    let access = match web.store(move |s| s.mcp_access(&key, now)).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "mcp token check failed: store unavailable");
            let mut r = (StatusCode::SERVICE_UNAVAILABLE, "credential store unavailable, retry\n").into_response();
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            return Err(Box::new(r));
        }
    };
    let Some(a) = access else {
        refuse("unknown token", "");
        return Err(Box::new(challenge(web, true)));
    };
    let g = &a.grant;
    let reason = if g.revoked_at.is_some() {
        Some("grant revoked")
    } else if g.expires_at <= now {
        Some("grant expired")
    } else if a.expires_at <= now {
        Some("token expired")
    } else if a.disabled {
        Some("user disabled")
    } else if a.audience != web.config().mcp_resource() {
        Some("wrong audience")
    } else {
        None
    };
    if let Some(reason) = reason {
        refuse(reason, &g.grant_id);
        return Err(Box::new(challenge(web, true)));
    }
    Ok(McpCaller {
        user_id: g.user_id,
        user: a.user,
        grant_id: g.grant_id.clone(),
        client_host: g.client_host.clone(),
        legacy: None,
        retry_after: None,
    })
}

/// A header value as the body would hold it: plain ASCII, or the `=?base64?...?=` sentinel.
fn header_value(headers: &HeaderMap, name: &str) -> Result<Option<String>, ()> {
    let Some(v) = headers.get(name) else { return Ok(None) };
    let v = v.to_str().map_err(|_| ())?;
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(encoded) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| ())?;
            String::from_utf8(bytes).map(Some).map_err(|_| ())
        }
        None => Ok(Some(v.to_string())),
    }
}

/// The mirrored headers of a modern request against its body (2026-07-28 Server Validation).
fn check_headers(headers: &HeaderMap, version: &str, method: &str, params: &Value) -> Result<(), String> {
    let get = |name: &str| header_value(headers, name).map_err(|()| format!("{name} header is malformed"));
    match get("mcp-protocol-version")? {
        None => return Err("missing MCP-Protocol-Version header".into()),
        Some(h) if h != version => {
            return Err(format!("MCP-Protocol-Version header '{h}' does not match body value '{version}'"));
        }
        Some(_) => {}
    }
    match get("mcp-method")? {
        None => return Err("missing Mcp-Method header".into()),
        Some(h) if h != method => return Err(format!("Mcp-Method header '{h}' does not match body value '{method}'")),
        Some(_) => {}
    }
    let named = match method {
        "tools/call" | "prompts/get" => Some(&params["name"]),
        "resources/read" => Some(&params["uri"]),
        _ => None,
    };
    if let Some(body) = named {
        match (get("mcp-name")?, body.as_str()) {
            (None, None) => {}
            (None, Some(_)) => return Err("missing Mcp-Name header".into()),
            (Some(h), b) if Some(h.as_str()) != b => {
                return Err(format!("Mcp-Name header '{h}' does not match body value '{}'", b.unwrap_or("")));
            }
            (Some(_), _) => {}
        }
    }
    Ok(())
}

async fn endpoint(State(st): State<McpState>, headers: HeaderMap, body: Bytes) -> Response {
    let started = Instant::now();
    if let Some(origin) = headers.get(header::ORIGIN)
        && !origin.to_str().is_ok_and(|o| allowed_origin(&st.web, o))
    {
        tracing::warn!(origin = ?origin, "mcp request from a foreign origin refused");
        return rpc_error(StatusCode::FORBIDDEN, &Value::Null, INVALID_REQUEST, "origin not allowed");
    }
    let mut caller = match gate(&st.web, &headers).await {
        Ok(c) => c,
        Err(refused) => return *refused,
    };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, &Value::Null, PARSE_ERROR, "parse error: not JSON"),
    };
    if msg.is_array() {
        return rpc_error(StatusCode::BAD_REQUEST, &Value::Null, INVALID_REQUEST, "batches are not supported");
    }
    let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_string) else {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return rpc_error(StatusCode::BAD_REQUEST, &id, INVALID_REQUEST, "invalid request: one JSON-RPC request or notification");
    };
    let Some(id) = msg.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let body_version = params["_meta"].get(META_VERSION);
    let modern = body_version.is_some();
    match body_version {
        Some(Value::String(v)) => {
            if let Err(why) = check_headers(&headers, v, &method, &params) {
                return rpc_error(StatusCode::BAD_REQUEST, &id, HEADER_MISMATCH, &format!("Header mismatch: {why}"));
            }
        }
        Some(_) => {}
        None if method == "initialize" => {}
        None => {
            let version = match header_value(&headers, "mcp-protocol-version") {
                Ok(v) => v.unwrap_or_else(|| HEADERLESS_VERSION.to_string()),
                Err(()) => {
                    return rpc_error(StatusCode::BAD_REQUEST, &id, HEADER_MISMATCH, "MCP-Protocol-Version header is malformed");
                }
            };
            if MODERN_VERSIONS.contains(&version.as_str()) {
                let why = format!("Header mismatch: MCP-Protocol-Version {version} needs params._meta[\"{META_VERSION}\"]");
                return rpc_error(StatusCode::BAD_REQUEST, &id, HEADER_MISMATCH, &why);
            }
            if !LEGACY_VERSIONS.contains(&version.as_str()) {
                let data = json!({"supported": supported_versions(), "requested": version});
                let body = error_response(&id, UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version", Some(data));
                return json_response(StatusCode::BAD_REQUEST, body);
            }
            caller.legacy = Some(version);
        }
    }
    let tool = (method == "tools/call").then(|| params["name"].as_str().unwrap_or("").to_string());
    if let Some(name) = &tool {
        let limits = &st.web.0.limits;
        caller.retry_after = match limits.tool_call(&caller.grant_id) {
            Err(wait) => Some(wait),
            Ok(()) if WRITE_TOOLS.contains(&name.as_str()) => limits.write(caller.user_id).err(),
            Ok(()) => None,
        };
    }
    let (dispatcher, who) = (st.dispatcher.clone(), caller.clone());
    let response = tokio::task::spawn_blocking(move || dispatcher.mcp(&who, msg)).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "mcp dispatcher panicked");
        error_response(&id, INTERNAL_ERROR, "the board did not answer; retry", None)
    });
    let code = response["error"]["code"].as_i64();
    let status = match code {
        Some(METHOD_NOT_FOUND) if modern => StatusCode::NOT_FOUND,
        Some(UNSUPPORTED_PROTOCOL_VERSION | HEADER_MISMATCH | PARSE_ERROR | INVALID_REQUEST) => StatusCode::BAD_REQUEST,
        _ => StatusCode::OK,
    };
    let outcome = match code {
        Some(c) => format!("error {c}"),
        None if response["result"]["isError"] == true => "tool error".into(),
        None => "ok".into(),
    };
    tracing::info!(user = %caller.user, grant = %caller.grant_id, client = %caller.client_host, %method,
        tool = tool.as_deref().unwrap_or(""), %outcome, ms = started.elapsed().as_millis() as u64, "mcp request");
    json_response(status, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn mirrored_headers_must_match_the_body() {
        let call = json!({"name": "send"});
        let ok = [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "send")];
        assert_eq!(check_headers(&headers(&ok), "2026-07-28", "tools/call", &call), Ok(()));
        let encoded = [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "=?base64?c2VuZA==?=")];
        assert_eq!(check_headers(&headers(&encoded), "2026-07-28", "tools/call", &call), Ok(()));
        for bad in [
            &ok[1..],
            &[ok[0], ok[1]][..],
            &[("mcp-protocol-version", "2025-06-18"), ok[1], ok[2]][..],
            &[ok[0], ("mcp-method", "tools/list"), ok[2]][..],
            &[ok[0], ok[1], ("mcp-name", "react")][..],
            &[ok[0], ok[1], ("mcp-name", "=?base64?!!?=")][..],
        ] {
            assert!(check_headers(&headers(bad), "2026-07-28", "tools/call", &call).is_err(), "{bad:?}");
        }
        // No name on a method without one.
        assert!(check_headers(&headers(&ok[..2]), "2026-07-28", "tools/list", &json!({})).is_err());
        let list = [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/list")];
        assert_eq!(check_headers(&headers(&list), "2026-07-28", "tools/list", &json!({})), Ok(()));
    }
}
