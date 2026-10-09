//! The web app's server side over the board's real stores: the app shell at `/`, the browser
//! session principal on `/v1/rpc` (CSRF, origin, op allow-list, the user's human as actor),
//! the conversation and fleet ops under DESIGN §3's visibility rules, attachment downloads and
//! the event socket. Each case runs on SQLite and, with CONDE_TEST_PG, on Postgres (where the
//! board events travel through LISTEN/NOTIFY). Sign-in goes through conde-remote's fake
//! OpenID provider.

#![allow(dead_code)]

#[path = "pg/mod.rs"]
mod pg;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use conde::dispatch::{Dispatcher, Principal};
use conde::protocol::Request;
use conde::web_store::WebBoard;
use conde_remote::web::{self, BridgeRegistry, WebConfig, WebSetup, WebState, WebStore};
use conde_remote::{NotificationSource, ServerConfig, ServerHandle};
use futures_util::{SinkExt, StreamExt};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, sha2::Sha256};
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;

include!("../../conde-remote/tests/support/web_harness.rs");

/// The user the fake provider signs in (from its preferred_username).
const USER: &str = "krisztian-dobmayer";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

async fn board(on_pg: bool) -> Option<Arc<Dispatcher>> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let url = if on_pg {
        match blocking(pg::fresh_database).await {
            Some(url) => Some(url),
            None => {
                eprintln!("skipped: set CONDE_TEST_PG to run the web app suite on Postgres");
                return None;
            }
        }
    } else {
        None
    };
    Some(Arc::new(
        blocking(move || match url {
            Some(url) => Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap(),
            None => Dispatcher::open_in_memory().unwrap(),
        })
        .await,
    ))
}

fn run<F: std::future::Future<Output = ()>>(on_pg: bool, case: impl FnOnce(Arc<Dispatcher>) -> F) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let Some(d) = rt.block_on(board(on_pg)) else { return };
    rt.block_on(case(d.clone()));
    drop(rt);
    drop(d);
}

macro_rules! on_both {
    ($($case:ident),* $(,)?) => {
        mod sqlite {
            $(#[test]
            fn $case() {
                super::run(false, super::$case);
            })*
        }
        mod postgres {
            $(#[test]
            fn $case() {
                super::run(true, super::$case);
            })*
        }
    };
}

on_both!(
    a_name_held_by_an_agent_session_is_never_taken_by_the_browser,
    two_names_on_one_human_address_both_stay_usable,
    the_browser_host_is_never_minted_as_a_real_host,
    a_connected_bridge_keeps_its_host_online,
    the_root_serves_the_app_to_a_signed_in_browser_only,
    browser_rpc_needs_the_session_its_csrf_token_and_this_origin,
    the_browser_acts_as_the_users_existing_human,
    a_user_without_a_human_gets_one_named_after_them,
    conversations_follow_dm_and_channel_visibility,
    delivery_state_shows_the_retry_clock_the_hold_and_the_last_error,
    attachments_download_only_where_the_message_is_visible,
    the_fleet_shows_online_discovery_and_own_delivery_health,
    the_event_socket_says_when_the_board_changed,
);

/// The whole server (bridge API, web sign-in, the app) over `d`, signed in through the fake
/// provider.
struct Ui {
    env: Env<WebBoard>,
    server: ServerHandle,
    d: Arc<Dispatcher>,
}

async fn ui(d: &Arc<Dispatcher>) -> Ui {
    ui_with(d, ServerConfig::default()).await
}

async fn ui_with(d: &Arc<Dispatcher>, config: ServerConfig) -> Ui {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let idp = start_idp().await;
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let base = format!("http://127.0.0.1:{port}");
    let mut entra = WebConfig::entra(TENANT, CLIENT_ID, CLIENT_SECRET, &format!("{base}/auth/callback")).unwrap();
    entra.issuer = idp.issuer.clone();
    let store = Arc::new(WebBoard::new(d.clone()));
    let web = WebSetup { entra: Some(entra), admin_key: None, store: store.clone() };
    let server = conde::cloud::start(
        d.clone(),
        format!("127.0.0.1:{port}").parse().unwrap(),
        ServerConfig { web: Some(web), ..config },
    )
    .await
    .unwrap();
    let env = Env {
        base,
        idp,
        store,
        registry: Arc::new(FakeRegistry::default()),
        clock: Arc::new(AtomicI64::new(START)),
    };
    Ui { env, server, d: d.clone() }
}

impl Ui {
    /// A browser signed in as the provider's current person, with its CSRF token.
    async fn browser(&self) -> (Browser, String) {
        let (b, csrf) = self.bare_browser().await;
        // The app's first call: names the human of a user who has none.
        self.rpc(&b, &csrf, "agent.whoami", json!({})).await;
        (b, csrf)
    }

    /// Signed in, with its CSRF token, before the app's first call.
    async fn bare_browser(&self) -> (Browser, String) {
        let b = Browser::new();
        b.sign_in(&self.env).await;
        let s = self.session(&b).await;
        (b, s["csrf"].as_str().unwrap().to_string())
    }

    async fn session(&self, b: &Browser) -> Value {
        b.follow.get(self.env.url("/v1/session")).send().await.unwrap().json().await.unwrap()
    }

    /// One browser RPC with the CSRF header and this origin.
    async fn rpc(&self, b: &Browser, csrf: &str, op: &str, body: Value) -> (StatusCode, Value) {
        let request = json!({"v": 1, "type": "request", "request_id": "r1", "op": op, "body": body,
                             "idempotency_key": conde::util::new_ulid()});
        let r = b
            .step
            .post(self.env.url("/v1/rpc"))
            .header("x-csrf-token", csrf)
            .header(header::ORIGIN, &self.env.base)
            .json(&request)
            .send()
            .await
            .unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }

    /// The result of a browser RPC that must succeed.
    async fn ok(&self, b: &Browser, csrf: &str, op: &str, body: Value) -> Value {
        let (status, v) = self.rpc(b, csrf, op, body).await;
        assert_eq!(status, StatusCode::OK, "{op}: {v}");
        assert_eq!(v["ok"], true, "{op}: {v}");
        v["result"].clone()
    }

    /// A principal of `user` on `host`, created on first sight.
    async fn principal(&self, user: &str, host: &str) -> Principal {
        let (d, user, host) = (self.d.clone(), user.to_string(), host.to_string());
        blocking(move || d.principal(&user, &host, false).unwrap()).await
    }

    /// One op through the dispatcher as `actor` (a CLI or bridge request).
    async fn op(&self, p: &Principal, actor: &str, op: &str, body: Value) -> Value {
        let (d, p) = (self.d.clone(), p.clone());
        let mut req = Request::new(conde::util::new_ulid(), op, body);
        req.actor = Some(actor.to_string());
        req.idempotency_key = Some(conde::util::new_ulid());
        let resp = blocking(move || d.dispatch(&p, &req)).await;
        assert!(resp.ok, "{op}: {:?}", resp.error);
        resp.result.unwrap()
    }
}

// --- the app shell ------------------------------------------------------------------------------

async fn the_root_serves_the_app_to_a_signed_in_browser_only(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let b = Browser::new();
    let r = b.step.get(ui.env.url("/")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/auth/login?next=%2F");
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    // Sign-in returns to the app.
    let (status, at, body) = b.get(&ui.env.url("/")).await;
    assert_eq!((status, at.as_str()), (StatusCode::OK, ui.env.url("/").as_str()), "{body}");
    assert!(body.starts_with("<!doctype html>"), "{body}");
    let r = b.step.get(ui.env.url("/")).send().await.unwrap();
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(r.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(b.step.get(ui.env.url("/app/nope.js")).send().await.unwrap().status(), StatusCode::NOT_FOUND);
    if conde_remote::web::app_embedded() {
        assert!(body.contains("/app/immutable/"), "{body}");
        let r = b.step.get(ui.env.url("/app/version.json")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-cache");
    }
    // The keys page stays where it was, linked from the app's user menu.
    assert_eq!(b.get(&ui.env.url("/keys")).await.0, StatusCode::OK);
    ui.server.shutdown().await.unwrap();
}

// --- the browser principal ----------------------------------------------------------------------

async fn browser_rpc_needs_the_session_its_csrf_token_and_this_origin(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let request = json!({"v": 1, "type": "request", "request_id": "r", "op": "agent.list", "body": {}});
    let rpc = ui.env.url("/v1/rpc");

    let anon = reqwest::Client::new();
    let r = anon.post(&rpc).json(&request).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "no bearer and no session");
    let r = anon.get(ui.env.url("/v1/session")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let (b, csrf) = ui.bare_browser().await;
    let s = ui.session(&b).await;
    assert_eq!(s["user"], USER);
    assert!(s["name"].is_null(), "a GET names nobody: {s}");
    assert_eq!(s["csrf_header"], "x-csrf-token");
    ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(ui.session(&b).await["name"], format!("{USER}/{USER}"));

    let r = b.step.post(&rpc).json(&request).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "no CSRF token");
    let r = b.step.post(&rpc).header("x-csrf-token", "0".repeat(64)).json(&request).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "a wrong CSRF token");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "stale_session", "the app reloads its session on this code: {v}");
    let r = b
        .step
        .post(&rpc)
        .header("x-csrf-token", &csrf)
        .header(header::ORIGIN, "https://evil.example")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "another origin");

    let list = ui.ok(&b, &csrf, "agent.list", json!({})).await;
    assert!(list["agents"].as_array().unwrap().iter().any(|a| a["name"] == format!("{USER}/{USER}")));

    // Only the human's ops: no registration, pins or bridge ops from a browser.
    for op in ["agent.register", "agent.pin", "agent.unregister", "bridge.claim", "doctor"] {
        let (status, v) = ui.rpc(&b, &csrf, op, json!({"name": "x"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{op}: {v}");
    }
    // An actor in the request is the server's to set, never the browser's.
    let request = json!({"v": 1, "type": "request", "request_id": "r", "op": "agent.whoami", "body": {},
                         "actor": "claude:someone-else@elsewhere"});
    let v: Value = b
        .step
        .post(&rpc)
        .header("x-csrf-token", &csrf)
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["result"]["name"], format!("{USER}/{USER}"), "{v}");
    assert_eq!(v["request_id"], "r");

    // A signed-out session is refused at once.
    let r = b.post_form(&ui.env.url("/auth/logout"), &[("csrf", csrf.as_str())]).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let (status, _) = ui.rpc(&b, &csrf, "agent.list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    ui.server.shutdown().await.unwrap();
}

async fn the_browser_acts_as_the_users_existing_human(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    // The first visit named the human after the user, on the browser's own host.
    let (b, csrf) = ui.browser().await;
    let me = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(me["address"], format!("human:{USER}@web"));
    // The human registers from a terminal on their laptop: the browser now writes as that name.
    let laptop = ui.principal(USER, "laptop").await;
    ui.op(&laptop, &format!("human:{USER}"), "agent.register", json!({"name": "krisztian"})).await;
    let me = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(me["name"], format!("{USER}/krisztian"));
    assert_eq!(me["address"], format!("human:{USER}@laptop"));
    // Two terminal names are ambiguous unless one is named after the user.
    let desk = ui.principal(USER, "desk-pc").await;
    ui.op(&desk, &format!("human:{USER}"), "agent.register", json!({"name": "kd"})).await;
    let (status, v) = ui.rpc(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["error"]["code"], "conflict", "{v}");
    let tower = ui.principal(USER, "tower").await;
    ui.op(&tower, &format!("human:{USER}"), "agent.register", json!({"name": USER})).await;
    let me = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(me["name"], format!("{USER}/{USER}"));
    assert_eq!(me["address"], format!("human:{USER}@tower"));
    ui.server.shutdown().await.unwrap();
}

async fn a_user_without_a_human_gets_one_named_after_them(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let me = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(me["name"], format!("{USER}/{USER}"));
    assert_eq!(me["address"], format!("human:{USER}@web"));
    // Stable across calls: the same name, no second registration.
    let again = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(again, me);
    ui.server.shutdown().await.unwrap();
}

// --- conversations ------------------------------------------------------------------------------

/// The user's agent `worker` (a Claude session on `k-host`) and another owner's agent
/// `david/scout`; returns (worker's principal and actor, scout's).
async fn cast(ui: &Ui) -> ((Principal, String), (Principal, String)) {
    let k = ui.principal(USER, "k-host").await;
    let worker = "claude:sess-worker".to_string();
    ui.op(&k, &worker, "agent.register", json!({"name": "worker"})).await;
    let dv = ui.principal("david", "d-host").await;
    let scout = "codex:thread-scout".to_string();
    ui.op(&dv, &scout, "agent.register", json!({"name": "scout"})).await;
    ((k, worker), (dv, scout))
}

async fn conversations_follow_dm_and_channel_visibility(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let human = format!("{USER}/{USER}");
    ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    let ((k, worker), (dv, scout)) = cast(&ui).await;

    let to_human = ui.op(&k, &worker, "message.send", json!({"to": {"agent": human}, "text": "done, see #team"})).await;
    ui.op(&dv, &scout, "message.send", json!({"to": {"agent": format!("{USER}/worker")}, "text": "private to worker"}))
        .await;
    ui.op(&k, &worker, "channel.create", json!({"channel": "#team"})).await;
    ui.op(&k, &worker, "channel.invite", json!({"channel": "#team", "name": human})).await;
    ui.op(&dv, &scout, "channel.create", json!({"channel": "#secret"})).await;
    ui.op(&dv, &scout, "message.send", json!({"to": {"channel": "#secret"}, "text": "members only"})).await;
    let post = ui.op(&k, &worker, "message.send", json!({"to": {"channel": "#team"}, "text": "status: green"})).await;

    let rail = ui.ok(&b, &csrf, "conversation.list", json!({})).await;
    assert_eq!(rail["me"], human);
    let dms = rail["dms"].as_array().unwrap();
    assert_eq!(dms.len(), 1, "{rail}");
    assert_eq!(dms[0]["with"], format!("{USER}/worker"));
    assert_eq!(dms[0]["unread"], 1);
    let channels: Vec<&Value> = rail["channels"].as_array().unwrap().iter().collect();
    assert_eq!(channels.len(), 1, "{rail}");
    assert_eq!(channels[0]["channel"], "#team");
    assert_eq!(channels[0]["unread"], 2, "the invite notice and the post");

    // The DM thread with worker: only the human's own DMs, never worker's DMs with others.
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let texts: Vec<&str> = h["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["done, see #team"]);
    assert_eq!(h["unread"], 1);
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "david/scout"})).await;
    assert!(h["messages"].as_array().unwrap().is_empty(), "{h}");
    // A channel by membership only.
    let (_, v) = ui.rpc(&b, &csrf, "conversation.history", json!({"channel": "#secret"})).await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    // Who is in a channel is public; how far each member has read is for members only.
    let m = ui.ok(&b, &csrf, "channel.members", json!({"channel": "#secret"})).await;
    assert_eq!(m["members"], json!([{"name": "david/scout"}]), "{m}");
    let m = ui.ok(&b, &csrf, "channel.members", json!({"channel": "#team"})).await;
    assert!(m["members"].as_array().unwrap().iter().all(|x| x["behind"].is_i64()), "{m}");
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"channel": "#team"})).await;
    let posts = h["messages"].as_array().unwrap();
    assert!(posts[0]["text"].as_str().unwrap().contains("was added to #team"), "{h}");
    assert_eq!(posts[1]["message_id"], post["message_id"]);
    assert!(h["read_seq"].as_i64().unwrap() < post["seq"].as_i64().unwrap());

    // Reading marks the DM and the channel read (and tells the browsers); a reply links to its
    // message.
    let mut wakes = ui.d.store().pending().events();
    ui.ok(&b, &csrf, "conversation.read", json!({"with": "worker", "up_to": to_human["seq"]})).await;
    assert!(next_board(&mut wakes).await, "a read that marks something announces it");
    let quiet = ui.ok(&b, &csrf, "conversation.read", json!({"with": "worker", "up_to": to_human["seq"]})).await;
    assert_eq!(quiet["marked"], 0);
    assert!(!next_board(&mut wakes).await, "a read that marks nothing wakes no browser");
    ui.ok(&b, &csrf, "conversation.read", json!({"channel": "#team", "up_to": i64::MAX})).await;
    let rail = ui.ok(&b, &csrf, "conversation.list", json!({})).await;
    assert_eq!(rail["dms"][0]["unread"], 0, "{rail}");
    assert_eq!(rail["channels"][0]["unread"], 0, "{rail}");
    let receipts = ui.op(&k, &worker, "receipt.list", json!({})).await;
    assert!(receipts.to_string().contains("read "), "{receipts}");
    let sent = ui
        .ok(&b, &csrf, "message.send", json!({"to": {"agent": "worker"}, "text": "thanks", "reply_to": to_human["message_id"]}))
        .await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let last = h["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["message_id"], sent["message_id"]);
    assert_eq!(last["reply_to"], to_human["message_id"]);
    assert_eq!(last["from"], human);

    // Paging back: `before` returns the older page in chronological order.
    for i in 0..3 {
        ui.op(&k, &worker, "message.send", json!({"to": {"agent": human}, "text": format!("n{i}")})).await;
    }
    let page = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker", "limit": 2})).await;
    assert_eq!(page["more"], true);
    let texts: Vec<&str> = page["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["n1", "n2"]);
    let before = page["messages"][0]["seq"].clone();
    let older = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker", "limit": 2, "before": before})).await;
    let texts: Vec<&str> = older["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["thanks", "n0"]);
    ui.server.shutdown().await.unwrap();
}

async fn delivery_state_shows_the_retry_clock_the_hold_and_the_last_error(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let ((k, worker), (dv, scout)) = cast(&ui).await;
    let sent = ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": "worker"}, "text": "ping"})).await;
    let state = |h: &Value| h["messages"].as_array().unwrap().last().unwrap()["delivery"]["recipients"][0].clone();

    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let r = state(&h);
    assert_eq!(r["recipient"], format!("{USER}/worker"));
    assert_eq!(r["state"], "queued", "{r}");
    assert!(r["retry_at"].is_null(), "no retry clock before a failure: {r}");

    // The host's bridge claims it and the session holds it for approval.
    let bridge = ui.d.bridge_principal(&k);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    assert_eq!(state(&h)["state"], "sending");
    ui.op(&bridge, "", "bridge.report", json!({"attempt_id": attempt, "outcome": "held"})).await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    assert_eq!(state(&h)["state"], "held");

    // The hold ends in an expiry: a retry is due after the backoff, with the error shown.
    ui.op(&bridge, "", "bridge.report", json!({"attempt_id": attempt, "outcome": "expired", "reason": "no answer in 10 min"}))
        .await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let r = state(&h);
    assert_eq!(r["state"], "retrying", "{r}");
    assert_eq!(r["attempts"], 1);
    assert_eq!(r["last_error"], json!({"status": "expired", "reason": "no answer in 10 min"}));
    let retry_at = r["retry_at"].as_str().unwrap().to_string();
    assert!(retry_at.as_str() > h["messages"][0]["date"].as_str().unwrap(), "{r}");

    // Read wins over the delivery state.
    ui.op(&k, &worker, "message.read", json!({"id": sent["message_id"]})).await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let r = state(&h);
    assert_eq!(r["state"], "read", "{r}");
    assert!(r["read_at"].is_string());

    // Another owner's message to the human shows only the human's own notification state.
    ui.op(&dv, &scout, "message.send", json!({"to": {"agent": format!("{USER}/{USER}")}, "text": "hi"})).await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "david/scout"})).await;
    let recipients = h["messages"][0]["delivery"]["recipients"].as_array().unwrap().clone();
    assert_eq!(recipients.len(), 1, "{h}");
    assert_eq!(recipients[0]["recipient"], format!("{USER}/{USER}"));
    assert_eq!(recipients[0]["state"], "unread", "a human's notification is the unread badge");
    ui.server.shutdown().await.unwrap();
}

// --- attachments --------------------------------------------------------------------------------

async fn attachments_download_only_where_the_message_is_visible(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let ((_k, _worker), _) = cast(&ui).await;
    let bytes = b"%PDF-1.7 not really\n\x00\x01";
    let sent = ui
        .ok(
            &b,
            &csrf,
            "message.send",
            json!({"to": {"agent": "worker"}, "text": "the report",
                   "attachments": [{"filename": "q3 \"final\" é.pdf", "data": STANDARD.encode(bytes)}]}),
        )
        .await;
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "worker"})).await;
    let att = h["messages"][0]["attachments"][0].clone();
    assert_eq!(att["filename"], "q3 \"final\" é.pdf");
    assert_eq!(att["size"], bytes.len());
    let url = ui.env.url(&format!("/v1/attachments/{}", att["attachment_id"].as_str().unwrap()));

    let r = b.step.get(&url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
        r.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"q3 _final_ _.pdf\"; filename*=UTF-8''q3%20%22final%22%20%C3%A9.pdf"
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), bytes);
    assert_eq!(sent["to"], format!("{USER}/worker"));

    // Signed out: refused; another user who is neither sender nor recipient: not found.
    assert_eq!(reqwest::get(&url).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    ui.env.idp.person.lock().unwrap().oid = "oid-david".into();
    ui.env.idp.person.lock().unwrap().upn = "dpacsuta@dakai.example".into();
    let (other, _) = ui.browser().await;
    let r = other.step.get(&url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = other.step.get(ui.env.url("/v1/attachments/not-an-id")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    ui.server.shutdown().await.unwrap();
}

// --- fleet ----------------------------------------------------------------------------------------

async fn the_fleet_shows_online_discovery_and_own_delivery_health(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let ((k, worker), (dv, _scout)) = cast(&ui).await;
    let agent = |f: &Value, name: &str| {
        f["agents"].as_array().unwrap().iter().find(|a| a["name"] == name).cloned().unwrap_or(Value::Null)
    };
    let host = |f: &Value, name: &str| {
        f["hosts"].as_array().unwrap().iter().find(|h| h["host"] == name).cloned().unwrap_or(Value::Null)
    };

    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(f["online_ttl_s"], 90);
    assert_eq!(host(&f, "k-host")["online"], false, "no bridge yet: {f}");
    assert_eq!(host(&f, "web")["bridge"], false, "the browser's human: {f}");
    let w = agent(&f, &format!("{USER}/worker"));
    assert_eq!((w["kind"].as_str(), w["discovered"].is_null(), w["own"].as_bool()), (Some("claude"), true, Some(true)));
    assert_eq!((w["held"].as_i64(), w["stuck"].as_i64()), (Some(0), Some(0)));
    let s = agent(&f, "david/scout");
    assert!(s["held"].is_null() && s["stuck"].is_null() && s["last_reached"].is_null(), "another owner's health: {s}");

    // The bridge connects (fence), reports worker's session live, then delivers a DM.
    let bridge = ui.d.bridge_principal(&k);
    ui.op(&bridge, "", "bridge.fence", json!({})).await;
    ui.op(&bridge, "", "bridge.endpoints", json!({"endpoints": [{"kind": "claude", "target": "sess-worker"}]})).await;
    ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": "worker"}, "text": "one"})).await;
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    // The delivery receipt comes 30 s after the claim: "reached" is the receipt's time.
    let claimed_at = ui.ok(&b, &csrf, "fleet.list", json!({})).await["now"].as_str().unwrap().to_string();
    ui.d.advance_clock(30_000);
    ui.op(&bridge, "", "bridge.report", json!({"attempt_id": attempt, "outcome": "delivered"})).await;
    let reported_at = ui.ok(&b, &csrf, "fleet.list", json!({})).await["now"].as_str().unwrap().to_string();
    // A second DM is held at the session.
    ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": "worker"}, "text": "two"})).await;
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let held = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    ui.op(&bridge, "", "bridge.report", json!({"attempt_id": held, "outcome": "held"})).await;

    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    let h = host(&f, "k-host");
    assert_eq!((h["online"].as_bool(), h["owner"].as_str()), (Some(true), Some(USER)), "{h}");
    let w = agent(&f, &format!("{USER}/worker"));
    assert!(w["discovered"].is_string(), "{w}");
    assert_eq!(w["held"], 1, "{w}");
    let reached = w["last_reached"].as_str().unwrap().to_string();
    assert!(reached > claimed_at && reached <= reported_at, "{reached} is the receipt's time, after {claimed_at}");

    // Without heartbeats the host drops offline after the TTL; one heartbeat brings it back.
    ui.d.advance_clock(91_000);
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(host(&f, "k-host")["online"], false, "{f}");
    let board = conde::cloud::Board::new(ui.d.clone());
    let instance = bridge.bridge_instance.clone().unwrap();
    blocking(move || board.heartbeat("k-host", &instance)).await;
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(host(&f, "k-host")["online"], true, "{f}");

    // An undelivered DM older than a minute is stuck (scout's host has no bridge).
    ui.op(&k, &worker, "message.send", json!({"to": {"agent": format!("{USER}/{USER}")}, "text": "to the human"})).await;
    ui.op(&dv, &_scout, "message.send", json!({"to": {"agent": format!("{USER}/worker")}, "text": "to worker"})).await;
    ui.d.advance_clock(61_000);
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(agent(&f, &format!("{USER}/worker"))["stuck"], 1, "{f}");
    assert_eq!(agent(&f, &format!("{USER}/{USER}"))["stuck"], 0, "a human is never stuck: {f}");
    ui.server.shutdown().await.unwrap();
}

// --- events -------------------------------------------------------------------------------------

async fn the_event_socket_says_when_the_board_changed(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let url = ui.env.url("/v1/events").replace("http://", "ws://");
    let cookie = b.jar_cookie(&ui.env.base);
    let handshake = |origin: Option<&str>, cookie: Option<&str>| {
        let mut req = tungstenite::client::IntoClientRequest::into_client_request(url.as_str()).unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert(header::ORIGIN, o.parse().unwrap());
        }
        if let Some(c) = cookie {
            req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
        }
        req
    };
    let refused = |r: Result<_, tungstenite::Error>| match r {
        Err(tungstenite::Error::Http(resp)) => resp.status(),
        Ok(_) => panic!("upgraded"),
        Err(e) => panic!("{e}"),
    };
    let base = ui.env.base.clone();
    assert_eq!(
        refused(tokio_tungstenite::connect_async(handshake(Some(&base), None)).await.map(|_| ())),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refused(tokio_tungstenite::connect_async(handshake(None, Some(&cookie))).await.map(|_| ())),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        refused(tokio_tungstenite::connect_async(handshake(Some("https://evil.example"), Some(&cookie))).await.map(|_| ())),
        StatusCode::FORBIDDEN
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(handshake(Some(&base), Some(&cookie))).await.unwrap();
    let hello = next_frame(&mut ws).await.expect("a hello");
    assert_eq!(hello["type"], "hello");

    ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": format!("{USER}/{USER}")}, "text": "note to self"})).await;
    let changed = loop {
        let f = next_frame(&mut ws).await.expect("a change");
        if f["type"] == "changed" && f["topics"].as_array().unwrap().contains(&json!("messages")) {
            break f;
        }
    };
    assert!(!changed.to_string().contains("note to self"), "events carry no content: {changed}");

    // A bridge outcome on another connection reaches the browser as a delivery change.
    let ((k, _worker), _) = cast(&ui).await;
    ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": "worker"}, "text": "x"})).await;
    let bridge = ui.d.bridge_principal(&k);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    // Everything up to the claim is announced and consumed first, so the next frame can only
    // be the report's.
    while quiet_frame(&mut ws).await.is_some() {}
    ui.op(&bridge, "", "bridge.report", json!({"attempt_id": attempt, "outcome": "delivered"})).await;
    let f = next_frame(&mut ws).await.expect("the report's change");
    assert_eq!(f["type"], "changed", "{f}");
    assert_eq!(f["topics"], json!(["delivery", "fleet"]), "{f}");
    ws.send(tungstenite::Message::Close(None)).await.unwrap();
    ui.server.shutdown().await.unwrap();
}

/// The next text frame unless the socket stays quiet for a moment.
async fn quiet_frame(ws: &mut Ws) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await?.ok()? {
                tungstenite::Message::Text(t) => return serde_json::from_str::<Value>(&t).ok(),
                _ => continue,
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(800), read).await.ok().flatten()
}

/// Whether a board wake arrives within a moment (other wakes skipped).
async fn next_board(wakes: &mut tokio::sync::broadcast::Receiver<conde::store::Wake>) -> bool {
    let wait = async {
        loop {
            if let Ok(conde::store::Wake::Board(_)) = wakes.recv().await {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(700), wait).await.is_ok()
}

async fn a_name_held_by_an_agent_session_is_never_taken_by_the_browser(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.bare_browser().await;
    // A GET names nobody.
    assert!(ui.session(&b).await["name"].is_null());
    let d2 = ui.d.clone();
    let bound = blocking(move || d2.with_conn(|c| c.bound_agents()).unwrap()).await;
    assert!(bound.is_empty(), "{bound:?}");
    // The user's own Claude session holds the name the browser would take.
    let k = ui.principal(USER, "k-host").await;
    ui.op(&k, "claude:sess-1", "agent.register", json!({"name": USER})).await;
    let (status, v) = ui.rpc(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("conde register"), "{v}");
    let me = ui.op(&k, "claude:sess-1", "agent.whoami", json!({})).await;
    assert_eq!(me["address"], "claude:sess-1@k-host", "the session keeps its name");
    // The race: the session registered after the browser read the registry. The browser's
    // registration re-checks in its own write transaction and refuses.
    let web = ui.principal(USER, "web-race").await;
    let (d2, mut p) = (ui.d.clone(), web.clone());
    p.host = "web".into();
    let mut req = Request::new("r", "agent.register", json!({"name": USER, "keep_sessions": true}));
    req.actor = Some(format!("human:{USER}@web"));
    req.idempotency_key = Some(conde::util::new_ulid());
    let resp = blocking(move || d2.dispatch(&p, &req)).await;
    assert_eq!(resp.error.map(|e| e.code), Some(conde::protocol::ErrorCode::Conflict));
    let me = ui.op(&k, "claude:sess-1", "agent.whoami", json!({})).await;
    assert_eq!(me["address"], "claude:sess-1@k-host", "the binding is untouched");
    ui.server.shutdown().await.unwrap();
}

async fn two_names_on_one_human_address_both_stay_usable(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.bare_browser().await;
    let laptop = ui.principal(USER, "laptop").await;
    ui.op(&laptop, &format!("human:{USER}"), "agent.register", json!({"name": "kd"})).await;
    ui.op(&laptop, &format!("human:{USER}"), "agent.register", json!({"name": USER})).await;
    // The terminal's own `whoami` sees two names on one address; the browser acts by name.
    let me = ui.ok(&b, &csrf, "agent.whoami", json!({})).await;
    assert_eq!(me["name"], format!("{USER}/{USER}"));
    let sent = ui.ok(&b, &csrf, "message.send", json!({"to": {"agent": "kd"}, "text": "to my other name"})).await;
    assert_eq!(sent["to"], format!("{USER}/kd"));
    let h = ui.ok(&b, &csrf, "conversation.history", json!({"with": "kd"})).await;
    assert_eq!(h["messages"][0]["from"], format!("{USER}/{USER}"));
    ui.server.shutdown().await.unwrap();
}

async fn the_browser_host_is_never_minted_as_a_real_host(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let b = Browser::new();
    let page = b.sign_in(&ui.env).await;
    let created = b.create_key(&ui.env, &csrf_of(&page), "web").await;
    assert!(created.contains("web-2"), "{created}");
    let d2 = ui.d.clone();
    let owner = blocking(move || d2.with_conn(|c| c.host_owner("web")).unwrap()).await;
    assert!(owner.is_none(), "{owner:?}");
    ui.server.shutdown().await.unwrap();
}

async fn a_connected_bridge_keeps_its_host_online(d: Arc<Dispatcher>) {
    let config = ServerConfig { ping_interval: Duration::from_millis(150), ..ServerConfig::default() };
    let ui = ui_with(&d, config).await;
    let (b, csrf) = ui.browser().await;
    let key = format!("conde_test_{}", conde::util::new_ulid());
    let digest = conde_remote::token_digest(&key);
    let d2 = ui.d.clone();
    blocking(move || {
        d2.with_tx(|c| conde::ops::hosts::insert_key(c, USER, "k-host", &digest, "test", "2026-10-09T00:00:00.000Z"))
            .unwrap()
    })
    .await;
    let mut cfg = conde_remote::BridgeConfig::new(format!("{}/v1/bridge", ui.env.base.replace("http", "ws")), &key, "k-host");
    cfg.backoff_min = Duration::from_millis(50);
    let bridge = conde_remote::BridgeClient::start(cfg, |_| {});
    let mut state = bridge.subscribe();
    tokio::time::timeout(Duration::from_secs(10), state.wait_for(|s| *s == conde_remote::BridgeState::Connected))
        .await
        .expect("the bridge connects")
        .unwrap();
    let online = |f: &Value| f["hosts"].as_array().unwrap().iter().find(|h| h["host"] == "k-host").unwrap()["online"].clone();
    // The client reports Connected once its hello is out; the server fences right after.
    let mut up = false;
    for _ in 0..50 {
        up = online(&ui.ok(&b, &csrf, "fleet.list", json!({})).await) == true;
        if up {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "the host comes online when its bridge connects");
    // Past the TTL of the connect-time stamp, answered pings alone keep the host online.
    ui.d.advance_clock(91_000);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(online(&ui.ok(&b, &csrf, "fleet.list", json!({})).await), true);
    bridge.shutdown();
    ui.server.shutdown().await.unwrap();
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The next text frame within a few seconds.
async fn next_frame(ws: &mut Ws) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await?.ok()? {
                tungstenite::Message::Text(t) => return serde_json::from_str::<Value>(&t).ok(),
                _ => continue,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read).await.ok().flatten()
}

impl Browser {
    /// The cookies this browser sends to `base`, as one Cookie header value.
    fn jar_cookie(&self, base: &str) -> String {
        use reqwest::cookie::CookieStore;
        let url: reqwest::Url = base.parse().unwrap();
        self.jar.cookies(&url).unwrap().to_str().unwrap().to_string()
    }
}
