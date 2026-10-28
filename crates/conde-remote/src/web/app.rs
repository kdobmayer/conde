//! The web app: the static Svelte build embedded in the binary (`/` and `/app/*`) and the
//! browser API under `/v1`, every route authenticated by the Entra browser session.
//!
//! - `GET /`: the app shell for a signed-in browser, else a redirect to sign in.
//! - `GET /app/{*path}`: the app's scripts and styles (no data, no session needed).
//! - `GET /v1/session`: who is signed in, and the CSRF token the app sends back.
//! - `POST /v1/rpc` (without a bearer token): one read for the signed-in owner (owner-scoped
//!   reads, the fleet, the directory; nothing writes); the `x-csrf-token` header must carry the
//!   session's token.
//! - `GET /v1/attachments/{id}`: one attachment as a download, under the same visibility rules.
//! - `GET /v1/events`: a WebSocket that says when the board changed (never what), so the app
//!   refetches what it shows.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{
    CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::cookie::CookieJar;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use rust_embed::Embed;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};
use tokio::time::{MissedTickBehavior, interval_at};

use super::session::{self, same_secret};
use super::{SessionRecord, WebState};
use crate::{Dispatcher, WebPrincipal, protocol};

/// The `npm run build` output of `web/`; empty when the binary was built without it.
#[derive(Embed)]
#[folder = "web-dist/"]
#[allow_missing = true]
struct Assets;

/// The header that carries the session's CSRF token on every browser RPC.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// The error code of a request whose CSRF token is not the session's (another sign-in replaced
/// the session): the app reloads `/v1/session` and retries once.
pub const STALE_SESSION: &str = "stale_session";
/// Every topic bit: sent when board events may have been missed.
pub const ALL_TOPICS: u8 = u8::MAX;
/// How long the event socket gathers board events before telling the browser.
const COALESCE: Duration = Duration::from_millis(250);
/// Ping period of the event socket (inside the ingress idle cut) and its session recheck.
const EVENTS_PING: Duration = Duration::from_secs(30);
/// The ops a browser may call: the owner-scoped reads, the fleet and public directory reads.
/// The web app is read-only: nothing that writes, registers or acts as an agent.
const BROWSER_OPS: &[&str] = &[
    "owner.list",
    "owner.history",
    "owner.message",
    "owner.feed",
    "owner.thread",
    "owner.search",
    "fleet.list",
    "agent.list",
    "channel.list",
    "channel.members",
];

/// What the app routes share with the server.
#[derive(Clone)]
pub(crate) struct AppState {
    pub web: WebState,
    pub dispatcher: Arc<dyn Dispatcher>,
    /// Board topic bits, from the store's events.
    pub events: broadcast::Sender<u8>,
    pub shutdown: watch::Receiver<bool>,
}

/// The app's routes (`POST /v1/rpc` stays with the server, which hands a request without a
/// bearer token to [`rpc`]; the server serves [`asset`] and [`favicon`] on every setup, since
/// its own pages use the icons).
pub(crate) fn routes(app: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/v1/session", get(session_info))
        .route("/v1/attachments/{id}", get(attachment))
        .route("/v1/events", get(events))
        .with_state(app)
}

fn no_store() -> (header::HeaderName, HeaderValue) {
    (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
}

/// A refusal in the dispatcher's response shape, so the app handles every failure alike.
fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [no_store()],
        axum::Json(protocol::error_response(None, code, message)),
    )
        .into_response()
}

fn signed_out() -> Response {
    api_error(StatusCode::UNAUTHORIZED, "unauthenticated", "not signed in")
}

fn store_failed(e: session::WebError) -> Response {
    tracing::error!(error = ?e, "browser session lookup failed");
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "internal",
        "the session store did not answer, retry",
    )
}

/// The session named by the request's cookie, or the response refusing it (boxed: a
/// `Response` is large).
async fn signed_in(app: &AppState, headers: &HeaderMap) -> Result<session::Session, Box<Response>> {
    match session::current(&app.web, &CookieJar::from_headers(headers)).await {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(Box::new(signed_out())),
        Err(e) => Err(Box::new(store_failed(e))),
    }
}

/// A browser request must come from the app's own origin when it names one (fetches and
/// WebSocket handshakes do); `required` refuses a request that names none.
fn same_origin(app: &AppState, headers: &HeaderMap, required: bool) -> bool {
    match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        Some(origin) => origin.eq_ignore_ascii_case(&app.web.config().public_url),
        None => !required,
    }
}

fn principal(record: &SessionRecord) -> WebPrincipal {
    WebPrincipal {
        user_id: record.user_id,
        user: record.user_name.clone(),
    }
}

/// Runs one browser request on a blocking thread; the response's request_id is the request's.
async fn dispatch(app: &AppState, user: WebPrincipal, request: Value) -> Value {
    let request_id = request.get("request_id").cloned().unwrap_or(Value::Null);
    let dispatcher = app.dispatcher.clone();
    let mut response = tokio::task::spawn_blocking(move || dispatcher.dispatch_web(&user, request))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "dispatcher panicked");
            protocol::error_response(None, "internal", "dispatcher failed")
        });
    if let Some(obj) = response.as_object_mut() {
        obj.insert("request_id".into(), request_id);
    }
    response
}

/// `POST /v1/rpc` from a browser: one read for the session's user. The CSRF token header is
/// required on every call.
pub(crate) async fn rpc(app: &AppState, headers: HeaderMap, body: Bytes) -> Response {
    if !same_origin(app, &headers, false) {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cross-origin request refused",
        );
    }
    let session = match signed_in(app, &headers).await {
        Ok(s) => s,
        Err(refused) => return *refused,
    };
    let token = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !same_secret(token, &session.record.csrf) {
        return api_error(
            StatusCode::FORBIDDEN,
            STALE_SESSION,
            "missing or stale CSRF token; reload the page",
        );
    }
    let mut request = match serde_json::from_slice::<Value>(&body) {
        Ok(request @ Value::Object(_)) => request,
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "body must be one JSON request object",
            );
        }
    };
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    if !BROWSER_OPS.contains(&op) {
        let message = format!("'{op}' is not available in the browser");
        return api_error(StatusCode::FORBIDDEN, "forbidden", &message);
    }
    if let Some(obj) = request.as_object_mut() {
        obj.remove("actor");
        obj.remove("from");
    }
    let response = dispatch(app, principal(&session.record), request).await;
    ([no_store()], axum::Json(response)).into_response()
}

/// `GET /v1/session`: who is signed in and the CSRF token for `/v1/rpc` and sign-out.
async fn session_info(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let session = match signed_in(&app, &headers).await {
        Ok(s) => s,
        Err(refused) => return *refused,
    };
    let r = &session.record;
    (
        [no_store()],
        axum::Json(json!({
            "user": r.user_name, "display": r.display, "csrf": r.csrf, "csrf_header": CSRF_HEADER,
        })),
    )
        .into_response()
}

/// `GET /v1/attachments/{id}`: the bytes as a download, if one of the user's agents can read
/// the message (`owner.attachment` decides).
async fn attachment(
    State(app): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match signed_in(&app, &headers).await {
        Ok(s) => s,
        Err(refused) => return *refused,
    };
    let request = json!({"v": 1, "type": "request", "request_id": "download", "op": "owner.attachment",
                         "body": {"attachment_id": id}});
    let response = dispatch(&app, principal(&session.record), request).await;
    if response["ok"] != true {
        let code = response["error"]["code"].as_str().unwrap_or("internal");
        let status = match code {
            "not_found" | "bad_request" => StatusCode::NOT_FOUND,
            "unauthenticated" | "forbidden" => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = response["error"]["message"]
            .as_str()
            .unwrap_or("attachment unavailable");
        return api_error(status, code, message);
    }
    let result = &response["result"];
    let Ok(bytes) =
        base64::engine::general_purpose::STANDARD.decode(result["data"].as_str().unwrap_or(""))
    else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "stored attachment unreadable",
        );
    };
    let filename = result["filename"].as_str().unwrap_or("attachment");
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_DISPOSITION, content_disposition(filename)),
            no_store(),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// `attachment; filename="<ascii>"; filename*=UTF-8''<percent-encoded>` (RFC 6266).
fn content_disposition(filename: &str) -> HeaderValue {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = filename
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// `GET /`: the app shell when signed in, else sign-in (which returns here).
async fn root(State(app): State<AppState>, headers: HeaderMap) -> Response {
    match session::current(&app.web, &CookieJar::from_headers(&headers)).await {
        Ok(Some(_)) => {}
        Ok(None) => return ([no_store()], Redirect::to("/auth/login?next=%2F")).into_response(),
        Err(e) => return e.into_response(),
    }
    let body = match Assets::get("index.html") {
        Some(file) => file.data.into_owned(),
        None => {
            b"<!doctype html><title>conde</title><p>This server was built without the web app. \
                  <a href=\"/keys\">Host keys</a></p>"
                .to_vec()
        }
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            no_store(),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("same-origin"),
            ),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        ],
        body,
    )
        .into_response()
}

/// The build's files under `app/`, listed once from the embed. A request is answered only with
/// one of these names, the same in a debug build (which reads web-dist from disk) as in release.
fn app_files() -> &'static HashSet<String> {
    static FILES: LazyLock<HashSet<String>> = LazyLock::new(|| {
        Assets::iter()
            .filter(|name| name.starts_with("app/"))
            .map(|name| name.into_owned())
            .collect()
    });
    &FILES
}

/// The build's file `app/<path>`, when `path` (decoded) is a plain relative name: no empty,
/// `.` or `..` segment, no leading `/`, no backslash or NUL.
fn app_file(path: &str) -> Option<rust_embed::EmbeddedFile> {
    let plain = !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    let name = format!("app/{path}");
    if !plain || !app_files().contains(&name) {
        return None;
    }
    Assets::get(&name)
}

/// `GET /app/{*path}`: one file of the build; hashed files and the fonts, whose names carry
/// their version, are cached for good.
pub(crate) async fn asset(Path(path): Path<String>) -> Response {
    let Some(file) = app_file(&path) else {
        return (StatusCode::NOT_FOUND, [no_store()], "not found\n").into_response();
    };
    let cache = if path.starts_with("immutable/") || path.starts_with("fonts/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mime = HeaderValue::from_str(file.metadata.mimetype())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

/// `GET /v1/events`: the board-change socket of a signed-in browser of this origin.
async fn events(
    State(app): State<AppState>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !same_origin(&app, &headers, true) {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cross-origin socket refused",
        );
    }
    let session = match signed_in(&app, &headers).await {
        Ok(s) => s,
        Err(refused) => return *refused,
    };
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(rejection) => return rejection.into_response(),
    };
    let subscription = app.events.subscribe();
    upgrade
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| run_events(app, session.id_digest, subscription, socket))
}

fn topic_names(bits: u8) -> Vec<&'static str> {
    [(1u8, "messages"), (2, "delivery"), (4, "fleet")]
        .into_iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| name)
        .collect()
}

async fn run_events(
    app: AppState,
    id_digest: String,
    mut board: broadcast::Receiver<u8>,
    socket: WebSocket,
) {
    let (mut tx, mut rx) = socket.split();
    let mut shutdown = app.shutdown.clone();
    let hello = protocol::frame("hello", &json!({"ping_s": EVENTS_PING.as_secs()}));
    if tx.send(Message::text(hello)).await.is_err() {
        return;
    }
    let mut ping = interval_at(tokio::time::Instant::now() + EVENTS_PING, EVENTS_PING);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pending: u8 = 0;
    let flush = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(flush);
    let close = |code: u16, reason: &'static str| {
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))
    };
    loop {
        tokio::select! {
            _ = protocol::shutdown_signal(&mut shutdown) => {
                let _ = tx.send(close(1001, "server shutdown")).await;
                return;
            }
            event = board.recv() => {
                let bits = match event {
                    Ok(bits) => bits,
                    Err(broadcast::error::RecvError::Lagged(_)) => ALL_TOPICS,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if pending == 0 {
                    flush.as_mut().reset(tokio::time::Instant::now() + COALESCE);
                }
                pending |= bits;
            }
            _ = &mut flush, if pending != 0 => {
                let frame = protocol::frame("changed", &json!({"topics": topic_names(pending)}));
                pending = 0;
                if tx.send(Message::text(frame)).await.is_err() {
                    return;
                }
            }
            _ = ping.tick() => {
                match session::lookup(&app.web, &id_digest, false).await {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        let _ = tx.send(close(4401, "signed out")).await;
                        return;
                    }
                    Err(e) => tracing::warn!(error = ?e, "event socket: session recheck failed"),
                }
                if tx.send(Message::Ping(Bytes::new())).await.is_err() {
                    return;
                }
            }
            msg = rx.next() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// The conde mark as inline SVG for the server's own pages (the web app's `mark.svg`, drawn
/// with the pages' colour tokens); empty when the binary was built without the web app.
pub(crate) fn mark_svg() -> &'static str {
    static MARK: LazyLock<String> = LazyLock::new(|| {
        Assets::get("app/mark.svg")
            .map(|f| String::from_utf8_lossy(&f.data).trim().to_string())
            .unwrap_or_default()
    });
    &MARK
}

/// `GET /favicon.ico`: the light 32 px PNG, for clients that ask for the icon by that name.
pub(crate) async fn favicon() -> Response {
    let Some(file) = Assets::get("app/favicon-light-32.png") else {
        return (StatusCode::NOT_FOUND, [no_store()], "not found\n").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

/// Whether this binary carries the built web app.
pub fn embedded() -> bool {
    Assets::get("index.html").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_of_the_build_are_served() {
        for path in [
            "",
            "/etc/passwd",
            "../Cargo.toml",
            "immutable/../../index.html",
            "./version.json",
            "a//b",
            "a\\b",
            "a\0b",
        ] {
            assert!(app_file(path).is_none(), "{path:?}");
        }
        // Whatever the build holds (none, or one older than a given file) is served by name.
        match app_files().iter().next() {
            Some(name) => assert!(app_file(&name["app/".len()..]).is_some(), "{name}"),
            None => eprintln!("no web build embedded: the lookup of a real file is not checked"),
        }
    }

    #[tokio::test]
    async fn every_font_the_styles_name_is_served_as_woff2_for_good() {
        let mut fonts = HashSet::new();
        for css in app_files().iter().filter(|n| n.ends_with(".css")) {
            let text = String::from_utf8_lossy(&Assets::get(css).unwrap().data).into_owned();
            for (at, _) in text.match_indices("/app/fonts/") {
                let rest = &text[at + "/app/".len()..];
                fonts.insert(rest[..rest.find(['\'', '"', ')']).unwrap()].to_string());
            }
        }
        if fonts.is_empty() {
            eprintln!("no web build with fonts embedded: the font checks are skipped");
            return;
        }
        assert_eq!(fonts.len(), 10, "{fonts:?}");
        for font in fonts {
            let r = asset(Path(font.clone())).await;
            assert_eq!(r.status(), StatusCode::OK, "{font}");
            assert_eq!(r.headers()[header::CONTENT_TYPE], "font/woff2", "{font}");
            assert_eq!(
                r.headers()[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable",
                "{font}"
            );
        }
    }

    #[test]
    fn content_disposition_quotes_and_encodes() {
        let v = content_disposition("rapport \"final\" é.pdf");
        assert_eq!(
            v.to_str().unwrap(),
            "attachment; filename=\"rapport _final_ _.pdf\"; filename*=UTF-8''rapport%20%22final%22%20%C3%A9.pdf"
        );
        assert_eq!(topic_names(5), vec!["messages", "fleet"]);
        assert_eq!(
            topic_names(ALL_TOPICS),
            vec!["messages", "delivery", "fleet"]
        );
    }
}
