//! The shared board's v2 server half (DESIGN-v2.md §6, §9, §11): migration of a v1 host,
//! mailboxes with route generations, the routes push, the admission window, the refusals for
//! a migrated host, non-disclosure and target-free listings. Every case runs the real server
//! (`conde::cloud::start`) over HTTP and raw bridge WebSockets, on SQLite and, with
//! CONDE_TEST_PG, on Postgres.

#![allow(dead_code)]

#[path = "pg/mod.rs"]
mod pg;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use conde::dispatch::Dispatcher;
use conde_remote::{ServerConfig, ServerHandle};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

const WAIT: Duration = Duration::from_secs(5);

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

async fn board(on_pg: bool) -> Option<Arc<Dispatcher>> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = if on_pg {
        match blocking(pg::fresh_database).await {
            Some(url) => Some(url),
            None => {
                eprintln!("skipped: set CONDE_TEST_PG to run the shared board suite on Postgres");
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
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let Some(d) = rt.block_on(board(on_pg)) else {
        return;
    };
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
    g8_the_preview_finds_candidates_in_message_relationships,
    g8_migration_publishes_the_list_fences_v1_and_replays_its_manifest,
    g8_a_v1_host_keeps_exchanging_messages_with_a_migrated_one,
    g5_an_unpublished_name_reads_like_one_that_never_existed,
    g15_unpublish_republish_and_takeover_move_the_route_generation,
    an_outcome_at_a_moved_generation_reattempts_at_the_current_route,
    a_late_delivered_at_a_moved_generation_reattempts_at_the_current_route,
    g11_the_window_bounds_claims_while_acks_and_routes_flow,
    a_migrated_host_refuses_session_addresses_and_v1_bridges,
    min_proto_2_refuses_every_v1_hello,
    listings_show_kind_and_host_never_a_target,
    a_mailbox_row_is_reached_only_when_delivered,
    board_info_names_the_board_account_and_host,
    a_held_mailbox_attempt_is_abandoned_when_the_mailbox_leaves_the_endpoints,
    a_hold_reported_after_its_mailbox_left_is_abandoned,
    a_hold_reported_after_its_mailbox_came_back_is_kept,
    the_route_revision_orders_snapshots_and_answers,
    a_takeover_abandons_only_open_attempts,
    a_read_hold_is_kept_when_the_mailbox_leaves_the_endpoints,
    a_late_outcome_at_a_moved_generation_keeps_a_read_notification_read,
);

// --- harness -----------------------------------------------------------------------------------

struct Srv {
    d: Arc<Dispatcher>,
    server: ServerHandle,
    addr: SocketAddr,
    http: reqwest::Client,
}

fn config() -> ServerConfig {
    ServerConfig {
        poll_interval: Duration::from_millis(50),
        ..ServerConfig::default()
    }
}

async fn srv(d: &Arc<Dispatcher>, config: ServerConfig) -> Srv {
    let server = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config)
        .await
        .unwrap();
    let addr = server.local_addr();
    Srv {
        d: d.clone(),
        server,
        addr,
        http: reqwest::Client::new(),
    }
}

/// A host credential: (user, host, key).
#[derive(Clone)]
struct Key {
    user: String,
    host: String,
    key: String,
}

impl Srv {
    async fn key(&self, user: &str, host: &str) -> Key {
        let key = format!("conde_test_{}", conde::util::new_ulid());
        let digest = conde_remote::token_digest(&key);
        let (d, u, h) = (self.d.clone(), user.to_string(), host.to_string());
        blocking(move || {
            d.with_tx(|tx| {
                conde::ops::hosts::insert_key(
                    tx,
                    &u,
                    &h,
                    &digest,
                    "test",
                    "2026-10-09T00:00:00.000Z",
                )
            })
            .unwrap()
        })
        .await;
        Key {
            user: user.into(),
            host: host.into(),
            key,
        }
    }

    /// One `/v1/rpc` request; the whole response object.
    async fn rpc(
        &self,
        k: &Key,
        actor: Option<&str>,
        op: &str,
        body: Value,
        idem: Option<&str>,
    ) -> Value {
        let mut req = json!({"v": 1, "type": "request", "request_id": conde::util::new_ulid(), "op": op, "body": body});
        if let Some(a) = actor {
            req["actor"] = json!(a);
        }
        if let Some(i) = idem {
            req["idempotency_key"] = json!(i);
        }
        self.http
            .post(format!("http://{}/v1/rpc", self.addr))
            .bearer_auth(&k.key)
            .json(&req)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// A mutation or query that must succeed; mutations get a fresh idempotency key.
    async fn ok(&self, k: &Key, actor: Option<&str>, op: &str, body: Value) -> Value {
        let r = self
            .rpc(k, actor, op, body.clone(), Some(&conde::util::new_ulid()))
            .await;
        assert_eq!(r["ok"], true, "{op} {body} as {actor:?}@{}: {r}", k.host);
        r["result"].clone()
    }

    /// A request that must fail: (code, message).
    async fn err(&self, k: &Key, actor: Option<&str>, op: &str, body: Value) -> (String, String) {
        let r = self
            .rpc(k, actor, op, body.clone(), Some(&conde::util::new_ulid()))
            .await;
        assert_eq!(
            r["ok"], false,
            "{op} {body} as {actor:?}@{} should fail: {r}",
            k.host
        );
        (
            r["error"]["code"].as_str().unwrap().into(),
            r["error"]["message"].as_str().unwrap().into(),
        )
    }

    async fn send(&self, k: &Key, actor: &str, to: &str, text: &str) -> String {
        let r = self
            .ok(
                k,
                Some(actor),
                "message.send",
                json!({"to": {"agent": to}, "text": text}),
            )
            .await;
        r["message_id"].as_str().unwrap().to_string()
    }

    async fn ws(&self, k: &Key) -> Ws {
        let mut req = format!("ws://{}/v1/bridge", self.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", k.key).parse().unwrap(),
        );
        let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        Ws {
            ws,
            buffer: VecDeque::new(),
            closed: false,
        }
    }

    /// A v1 bridge of `k`'s host, fenced in, with harness endpoints.
    async fn v1(&self, k: &Key, instance: &str, endpoints: Value) -> Ws {
        let mut ws = self.ws(k).await;
        ws.send(json!({"v": 1, "type": "hello", "host_id": k.host, "bridge_instance": instance, "endpoints": endpoints}))
            .await;
        ws.wait_fenced_in(&self.d, instance).await;
        ws
    }

    /// A v2 bridge of `k`'s host with its window and mailbox endpoints, fenced in.
    async fn v2(&self, k: &Key, instance: &str, window: u32, mailboxes: &[&str]) -> Ws {
        let mut ws = self.ws(k).await;
        let endpoints: Vec<Value> = mailboxes
            .iter()
            .map(|m| json!({"kind": "mailbox", "mailbox_id": m}))
            .collect();
        ws.send(
            json!({"v": 1, "type": "hello", "host_id": k.host, "bridge_instance": instance,
                       "endpoints": endpoints, "proto": 2, "window": window}),
        )
        .await;
        ws.wait_fenced_in(&self.d, instance).await;
        ws
    }

    async fn attempts(&self) -> Vec<conde::store::AttemptRow> {
        let d = self.d.clone();
        blocking(move || d.with_conn(|t| t.attempts()).unwrap()).await
    }

    async fn agent(&self, owner: &str, name: &str) -> conde::store::Agent {
        let (d, owner, name) = (self.d.clone(), owner.to_string(), name.to_string());
        blocking(move || {
            d.with_conn(|t| {
                let uid = t.user_id(&owner)?.unwrap();
                t.agent_by_name(uid, &name)
            })
            .unwrap()
            .unwrap()
        })
        .await
    }

    async fn migrate(&self, k: &Key, publish: &[&str], idem: &str) -> Value {
        let r = self
            .rpc(
                k,
                None,
                "host.migrate_v2",
                json!({"publish": publish}),
                Some(idem),
            )
            .await;
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    }
}

struct Ws {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    buffer: VecDeque<Value>,
    closed: bool,
}

impl Ws {
    async fn send(&mut self, v: Value) {
        self.ws
            .send(tungstenite::Message::text(v.to_string()))
            .await
            .unwrap();
    }

    /// The next text frame within `limit`; None on timeout or close.
    async fn read(&mut self, limit: Duration) -> Option<Value> {
        if self.closed {
            return None;
        }
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Err(_) => return None,
                Ok(Some(Ok(tungstenite::Message::Text(t)))) => {
                    return Some(serde_json::from_str(&t).unwrap());
                }
                Ok(Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None) => {
                    self.closed = true;
                    return None;
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// The next frame of `kind`, keeping frames of other kinds for later.
    async fn next(&mut self, kind: &str) -> Value {
        if let Some(i) = self.buffer.iter().position(|f| f["type"] == kind) {
            return self.buffer.remove(i).unwrap();
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let f = self
                .read(left)
                .await
                .unwrap_or_else(|| panic!("no {kind} frame (buffered: {:?})", self.buffer));
            if f["type"] == kind {
                return f;
            }
            self.buffer.push_back(f);
        }
    }

    /// No frame of `kind` arrives within `quiet`.
    async fn none(&mut self, kind: &str, quiet: Duration) {
        assert!(
            !self.buffer.iter().any(|f| f["type"] == kind),
            "unexpected {kind}: {:?}",
            self.buffer
        );
        let deadline = tokio::time::Instant::now() + quiet;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            match self.read(left).await {
                None => return,
                Some(f) if f["type"] == kind => panic!("unexpected {kind}: {f}"),
                Some(f) => self.buffer.push_back(f),
            }
        }
    }

    /// The connection's error frame and its close.
    async fn refused(&mut self) -> Value {
        let err = self.next("error").await;
        assert_eq!(self.read(WAIT).await, None, "closed after the error");
        err
    }

    async fn outcome(
        &mut self,
        attempt: &str,
        seq: u64,
        status: &str,
        reason: Option<&str>,
    ) -> Value {
        self.send(json!({"v": 1, "type": "outcome", "attempt_id": attempt, "event_seq": seq, "status": status,
                         "reason": reason}))
            .await;
        let ack = self.next("outcome_ack").await;
        assert_eq!(ack["attempt_id"], attempt, "{ack}");
        ack
    }

    async fn wait_fenced_in(&mut self, d: &Arc<Dispatcher>, instance: &str) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let (d, i) = (d.clone(), instance.to_string());
            let state = blocking(move || d.with_conn(|t| t.bridge_instance(&i)).unwrap()).await;
            if state.is_some_and(|(_, s)| s == "active") {
                return;
            }
            if let Some(f) = self.buffer.iter().find(|f| f["type"] == "error") {
                panic!("hello of {instance} refused: {f}");
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{instance} never fenced in"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn eventually<F: std::future::Future<Output = bool>>(
    what: &str,
    mut check: impl FnMut() -> F,
) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Ws {
    /// The notify of `message_id`; notifies of other messages on the way are delivered.
    async fn notify_for(&mut self, message_id: &str) -> Value {
        loop {
            let n = self.next("notify").await;
            if n["message_id"] == message_id {
                return n;
            }
            self.outcome(n["attempt_id"].as_str().unwrap(), 1, "delivered", None)
                .await;
        }
    }
}

// --- the fixture: kdobmayer's v1 host homepc, ben's v1 host benbox ------------------------------

const DESK: &str = "claude:s-desk-5f2c";
const WORKER: &str = "codex:t-worker-91aa";
const LONELY: &str = "claude:s-lonely-77d0";
const KD: &str = "human:kd";
const RELAY: &str = "codex:t-relay-3b1e";

struct Fx {
    s: Srv,
    kd: Key,
    ben: Key,
}

/// homepc (kd): desk (Claude, pinned by the human), worker (Codex), lonely (Claude); kd's human
/// name kd/human is the board's own (bound to the web host). benbox (ben): relay (Codex). desk and relay exchange DMs and both are in #dev, where desk posts; one desk
/// post in #dev is an imported row (no notification, dated before a notified message).
async fn fixture(d: &Arc<Dispatcher>, config: ServerConfig) -> Fx {
    let s = srv(d, config).await;
    let kd = s.key("kd", "homepc").await;
    let ben = s.key("ben", "benbox").await;
    s.ok(&kd, Some(DESK), "agent.register", json!({"name": "desk"}))
        .await;
    s.ok(
        &kd,
        Some(WORKER),
        "agent.register",
        json!({"name": "worker"}),
    )
    .await;
    s.ok(
        &kd,
        Some(LONELY),
        "agent.register",
        json!({"name": "lonely"}),
    )
    .await;
    s.ok(
        &kd,
        Some(KD),
        "agent.pin",
        json!({"name": "desk", "address": DESK}),
    )
    .await;
    s.ok(
        &ben,
        Some(RELAY),
        "agent.register",
        json!({"name": "relay"}),
    )
    .await;
    s.send(&ben, RELAY, "kd/desk", "hi desk").await;
    s.send(&kd, DESK, "ben/relay", "hi relay").await;
    s.send(&kd, WORKER, "lonely", "same owner only").await;
    s.ok(
        &kd,
        Some(DESK),
        "channel.create",
        json!({"channel": "#dev"}),
    )
    .await;
    s.ok(
        &ben,
        Some(RELAY),
        "channel.join",
        json!({"channel": "#dev"}),
    )
    .await;
    s.ok(
        &kd,
        Some(DESK),
        "message.send",
        json!({"to": {"channel": "#dev"}, "text": "a post"}),
    )
    .await;
    // An imported post: what `conde import` writes (kond's old date, no notification).
    let desk = s.agent("kd", "desk").await;
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(|tx| {
            let dev = tx.channel_by_name("dev")?.unwrap();
            tx.insert_message(&conde::store::NewMessage {
                id: &conde::util::new_ulid(),
                sender_id: &desk.id,
                recipient_id: None,
                channel_id: Some(&dev.id),
                text: "from kond",
                reply_to: None,
                created_at: "2026-08-01T00:00:00.000Z",
            })?;
            Ok(())
        })
        .unwrap()
    })
    .await;
    Fx { s, kd, ben }
}

/// The agent id of `kd/<name>`.
async fn id(s: &Srv, name: &str) -> String {
    s.agent("kd", name).await.id
}

/// homepc migrated with `publish` (names), connected as a v2 bridge with `window`.
async fn migrated(fx: &Fx, publish: &[&str], window: u32) -> (Ws, Vec<String>) {
    let mut ids = Vec::new();
    for n in publish {
        ids.push(id(&fx.s, n).await);
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    fx.s.migrate(&fx.kd, &refs, &conde::util::new_ulid()).await;
    let ws =
        fx.s.v2(
            &fx.kd,
            &format!("v2-{}", conde::util::new_ulid()),
            window,
            &refs,
        )
        .await;
    (ws, ids)
}

fn mailbox(id: &str) -> String {
    format!("mailbox:{id}@homepc")
}

fn routes_of(frame: &Value) -> Vec<(String, u64)> {
    let mut r: Vec<(String, u64)> = frame["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["mailbox_id"].as_str().unwrap().to_string(),
                m["route_generation"].as_u64().unwrap(),
            )
        })
        .collect();
    r.sort();
    r
}

// --- G8 ---------------------------------------------------------------------------------------

async fn g8_the_preview_finds_candidates_in_message_relationships(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let p =
        fx.s.ok(&fx.kd, None, "host.migrate_preview", json!({}))
            .await;
    let preview: conde::protocol::v2::MigratePreview = serde_json::from_value(p.clone()).unwrap();
    let names: Vec<&str> = preview.rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        ["desk", "lonely", "worker"],
        "every bound row of this host, nothing of benbox nor the web host"
    );
    let row = |n: &str| preview.rows.iter().find(|r| r.name == n).unwrap().clone();
    let desk = row("desk");
    assert!(desk.candidate);
    assert_eq!(
        (desk.address.as_str(), desk.pin.as_deref()),
        (&*format!("{DESK}@homepc"), Some(&*format!("{DESK}@homepc")))
    );
    let ev: Vec<(String, String, String, u64)> = desk
        .evidence
        .iter()
        .map(|e| {
            let j = serde_json::to_value(e).unwrap();
            (
                j["via"].as_str().unwrap().into(),
                e.with.clone(),
                j["source"].as_str().unwrap().into(),
                e.messages,
            )
        })
        .collect();
    assert_eq!(
        ev,
        [
            ("channel".into(), "#dev".into(), "imported".into(), 1),
            ("channel".into(), "#dev".into(), "later".into(), 1),
            ("dm".into(), "ben/relay".into(), "later".into(), 2),
        ],
        "{p}"
    );
    assert!(desk.evidence.iter().all(|e| !e.last_at.is_empty()));
    for n in ["lonely", "worker"] {
        let r = row(n);
        assert!(
            !r.candidate && r.evidence.is_empty(),
            "a same-owner DM is no evidence: {r:?}"
        );
    }
    // Read-only: nothing changed, and ben's host sees only its own row.
    assert_eq!(
        fx.s.agent("kd", "desk").await.address.unwrap().to_string(),
        format!("{DESK}@homepc")
    );
    let theirs =
        fx.s.ok(&fx.ben, None, "host.migrate_preview", json!({}))
            .await;
    assert_eq!(theirs["rows"].as_array().unwrap().len(), 1, "{theirs}");
    assert_eq!(
        theirs["rows"][0]["candidate"], true,
        "relay has DMs with kd/desk and posts nowhere: {theirs}"
    );
}

async fn g8_migration_publishes_the_list_fences_v1_and_replays_its_manifest(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let mut old = s
        .v1(
            &fx.kd,
            "v1-homepc",
            json!([{"kind": "claude", "session_id": "s-desk-5f2c"}]),
        )
        .await;
    eventually("the v1 bridge's session recorded", || {
        let d = d.clone();
        async move {
            blocking(move || d.with_conn(|t| t.endpoint_rows()).unwrap())
                .await
                .iter()
                .any(|e| e.host == "homepc")
        }
    })
    .await;
    let desk = id(s, "desk").await;
    let before = s.agent("kd", "desk").await.binding_version;
    let key = conde::util::new_ulid();
    let manifest = s.migrate(&fx.kd, &[&desk], &key).await;

    // The connected v1 bridge of the host is fenced and closed.
    assert_eq!(old.refused().await["code"], "fenced");
    // Exactly the confirmed list is published; the other names are unbound.
    let m: conde::protocol::v2::Manifest = serde_json::from_value(manifest.clone()).unwrap();
    let rows: Vec<(&str, &str, Option<&str>, bool)> = m
        .bindings
        .iter()
        .map(|b| {
            (
                b.name.as_str(),
                b.address.as_str(),
                b.pin.as_deref(),
                b.published.is_some(),
            )
        })
        .collect();
    let (desk_at, worker_at, lonely_at) = (
        format!("{DESK}@homepc"),
        format!("{WORKER}@homepc"),
        format!("{LONELY}@homepc"),
    );
    assert_eq!(
        rows,
        [
            ("desk", desk_at.as_str(), Some(desk_at.as_str()), true),
            ("lonely", lonely_at.as_str(), None, false),
            ("worker", worker_at.as_str(), None, false),
        ]
    );
    let desk_row = s.agent("kd", "desk").await;
    assert_eq!(desk_row.address.unwrap().to_string(), mailbox(&desk));
    assert_eq!(
        (desk_row.binding_version, desk_row.pinned),
        (before + 1, None),
        "a new generation, no pin"
    );
    let route = m.bindings[0].published.clone().unwrap();
    assert_eq!(
        (route.mailbox_id.as_str(), route.route_generation as i64),
        (desk.as_str(), before + 1)
    );
    assert_eq!(
        s.agent("kd", "human").await.address.unwrap().to_string(),
        "human:kd@web",
        "the human name is not the host's"
    );
    assert!(
        s.agent("kd", "lonely").await.address.is_none()
            && s.agent("kd", "worker").await.address.is_none()
    );
    // No harness address and no endpoint row left for the host, and the host is mailbox-only.
    let (agents, endpoints) = blocking({
        let d = d.clone();
        move || {
            d.with_conn(|t| {
                Ok((
                    t.owner_agents(t.user_id("kd")?.unwrap())?,
                    t.endpoint_rows()?,
                ))
            })
            .unwrap()
        }
    })
    .await;
    let mailbox_only = s.ok(&fx.kd, None, "board.info", json!({})).await["mailbox_only"] == true;
    for a in &agents {
        let harness = |s: &str| s.starts_with("claude:") || s.starts_with("codex:");
        assert!(
            !a.address.as_ref().is_some_and(|x| harness(&x.to_string())),
            "{a:?}"
        );
        assert!(
            !a.pinned.as_deref().is_some_and(|p| p.ends_with("@homepc")),
            "{a:?}"
        );
    }
    assert!(
        !endpoints.iter().any(|e| e.host == "homepc"),
        "{endpoints:?}"
    );
    assert!(mailbox_only);

    // The stored answer replays identically under the same key (after a kill before the local apply).
    let replay = s.migrate(&fx.kd, &[&desk], &key).await;
    assert_eq!(replay, manifest);
    // A new key on a migrated host is refused, and changes nothing.
    let r = s
        .rpc(
            &fx.kd,
            None,
            "host.migrate_v2",
            json!({"publish": []}),
            Some("another"),
        )
        .await;
    assert_eq!(r["error"]["code"], "conflict", "{r}");
    assert_eq!(
        s.agent("kd", "desk").await.address.unwrap().to_string(),
        mailbox(&desk)
    );
    // A name that is not bound on the host is refused before anything changes.
    let ben_relay = s.agent("ben", "relay").await.id;
    let r = s
        .rpc(
            &fx.ben,
            None,
            "host.migrate_v2",
            json!({"publish": [desk]}),
            Some("k1"),
        )
        .await;
    assert_eq!(r["error"]["code"], "bad_request", "{r}");
    assert!(
        s.agent("ben", "relay").await.address.is_some(),
        "{ben_relay} untouched"
    );
}

async fn g8_a_v1_host_keeps_exchanging_messages_with_a_migrated_one(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let mut ben = s
        .v1(
            &fx.ben,
            "v1-benbox",
            json!([{"kind": "codex", "thread_id": "t-relay-3b1e"}]),
        )
        .await;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = &ids[0];
    let generation = s.agent("kd", "desk").await.binding_version as u64;
    assert_eq!(
        routes_of(&home.next("routes").await),
        [(desk.clone(), generation)],
        "routes on connect"
    );
    // The re-attempted DM from before the migration arrives at the mailbox, then a new one.
    let first = home.next("notify").await;
    home.outcome(first["attempt_id"].as_str().unwrap(), 1, "delivered", None)
        .await;
    let id = s.send(&fx.ben, RELAY, "kd/desk", "still there?").await;
    let n = home.notify_for(&id).await;
    assert_eq!(
        n["endpoint"],
        json!({"kind": "mailbox", "mailbox_id": desk})
    );
    assert_eq!(n["binding_version"].as_u64(), Some(generation));
    assert_eq!(n["text"], "", "a v2 notify carries no text");
    assert_eq!(
        (
            n["kind"].clone(),
            n["sender"].clone(),
            n["place"].clone(),
            n["member"].clone()
        ),
        (json!("dm"), json!("ben/relay"), json!("dm"), json!(true))
    );
    assert!(n["badges"].is_array(), "{n}");
    home.outcome(n["attempt_id"].as_str().unwrap(), 1, "delivered", None)
        .await;
    // The mailbox answers as itself; the v1 host gets a v1 notify with its text.
    let reply = s.send(&fx.kd, &mailbox(desk), "ben/relay", "yes").await;
    let back = ben.notify_for(&reply).await;
    assert_eq!(
        back["endpoint"],
        json!({"kind": "codex", "thread_id": "t-relay-3b1e"})
    );
    assert!(
        back["text"]
            .as_str()
            .unwrap()
            .contains(&format!("from kd/desk. Read it with: conde read {reply}"))
    );
    assert!(
        back.get("kind").is_none(),
        "no structured fields on v1: {back}"
    );
    ben.outcome(back["attempt_id"].as_str().unwrap(), 1, "delivered", None)
        .await;
    let read = s
        .ok(&fx.ben, Some(RELAY), "message.read", json!({"id": reply}))
        .await;
    assert_eq!(read["messages"][0]["from"], "kd/desk", "{read}");
    // A channel post with a mention reaches the mailbox with the place and membership.
    s.ok(
        &fx.ben,
        Some(RELAY),
        "message.send",
        json!({"to": {"channel": "#dev"}, "text": "@kd/desk look"}),
    )
    .await;
    let m = home.next("notify").await;
    assert_eq!(
        (m["kind"].clone(), m["place"].clone(), m["member"].clone()),
        (json!("mention"), json!("#dev"), json!(true))
    );
}

// --- G5 ---------------------------------------------------------------------------------------

async fn g5_an_unpublished_name_reads_like_one_that_never_existed(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, _) = migrated(&fx, &["desk"], 8).await;
    home.next("routes").await;
    let queued_for_lonely = || {
        let d = d.clone();
        blocking(move || {
            d.with_conn(|t| t.notifications())
                .unwrap()
                .iter()
                .filter(|n| n.recipient_name == "kd/lonely")
                .count()
        })
    };
    let before = queued_for_lonely().await;
    // lonely was registered and is now unpublished; ghost never existed.
    for (op, body) in [
        (
            "message.send",
            json!({"to": {"agent": "kd/NAME"}, "text": "hello?"}),
        ),
        ("agent.resolve", json!({"name": "kd/NAME"})),
        (
            "channel.invite",
            json!({"channel": "#dev", "name": "kd/NAME"}),
        ),
    ] {
        let ask =
            |n: &str| serde_json::from_str::<Value>(&body.to_string().replace("NAME", n)).unwrap();
        let lonely = s.err(&fx.ben, Some(RELAY), op, ask("lonely")).await;
        let ghost = s.err(&fx.ben, Some(RELAY), op, ask("ghost")).await;
        assert_eq!(
            lonely,
            (
                "not_found".into(),
                "recipient unavailable: kd/lonely".into()
            ),
            "{op}"
        );
        assert_eq!(
            ghost,
            ("not_found".into(), "recipient unavailable: kd/ghost".into()),
            "{op}"
        );
    }
    let lonely = s
        .err(
            &fx.ben,
            Some(RELAY),
            "profile.get",
            json!({"name": "kd/lonely"}),
        )
        .await;
    let ghost = s
        .err(
            &fx.ben,
            Some(RELAY),
            "profile.get",
            json!({"name": "kd/ghost"}),
        )
        .await;
    assert_eq!(
        (lonely.0, lonely.1.replace("lonely", "X")),
        (ghost.0, ghost.1.replace("ghost", "X"))
    );
    // Nothing was queued for the unpublished name, and nothing reaches the host for it.
    assert_eq!(queued_for_lonely().await, before);
    let desk_first = home.next("notify").await; // the DM from before the migration, at the mailbox
    assert_eq!(desk_first["recipient"], "kd/desk");
    home.none("notify", Duration::from_millis(300)).await;
}

// --- G15 --------------------------------------------------------------------------------------

async fn g15_unpublish_republish_and_takeover_move_the_route_generation(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    let g1 = s.agent("kd", "desk").await.binding_version as u64;
    assert_eq!(routes_of(&home.next("routes").await), [(desk.clone(), g1)]);
    let pre = home.next("notify").await; // the DM from before the migration
    home.outcome(pre["attempt_id"].as_str().unwrap(), 1, "delivered", None)
        .await;

    // Unpublish at the current generation: the route is gone, a remote send is refused.
    let u = s
        .ok(
            &fx.kd,
            None,
            "mailbox.unpublish",
            json!({"mailbox_id": desk, "expected_generation": g1}),
        )
        .await;
    assert_eq!(
        unrevised(u),
        json!({"mailbox_id": desk, "route_generation": g1 + 1})
    );
    assert_eq!(routes_of(&home.next("routes").await), []);
    let refused = s
        .err(
            &fx.ben,
            Some(RELAY),
            "message.send",
            json!({"to": {"agent": "kd/desk"}, "text": "x"}),
        )
        .await;
    assert_eq!(
        refused,
        ("not_found".into(), "recipient unavailable: kd/desk".into())
    );
    home.none("notify", Duration::from_millis(300)).await;
    // A stale unpublish conflicts.
    let (code, _) = s
        .err(
            &fx.kd,
            None,
            "mailbox.unpublish",
            json!({"mailbox_id": desk, "expected_generation": g1}),
        )
        .await;
    assert_eq!(code, "conflict");

    // Re-publishing the same name: the same mailbox at a new generation; messages flow again.
    let p = s
        .ok(&fx.kd, None, "mailbox.publish", json!({"name": "desk"}))
        .await;
    let g3 = g1 + 2;
    assert_eq!(
        unrevised(p.clone()),
        json!({"name": "desk", "mailbox_id": desk, "route_generation": g3})
    );
    assert_eq!(
        s.ok(&fx.kd, None, "mailbox.publish", json!({"name": "desk"}))
            .await,
        p,
        "again: a no-op"
    );
    assert_eq!(routes_of(&home.next("routes").await), [(desk.clone(), g3)]);
    let mine = s.ok(&fx.kd, None, "mailbox.mine", json!({})).await;
    assert_eq!(
        unrevised(mine),
        json!({"mailboxes": [{"name": "desk", "mailbox_id": desk, "route_generation": g3}]})
    );
    let id = s.send(&fx.ben, RELAY, "kd/desk", "back?").await;
    let n = home.next("notify").await;
    assert_eq!(
        (n["message_id"].as_str(), n["binding_version"].as_u64()),
        (Some(id.as_str()), Some(g3))
    );
    // Left without an outcome: the takeover must re-attempt it at the new host.

    // Another host of the same owner takes the name over.
    let laptop = s.key("kd", "laptop").await;
    s.migrate(&laptop, &[], &conde::util::new_ulid()).await;
    let (code, _) = s
        .err(&laptop, None, "mailbox.publish", json!({"name": "desk"}))
        .await;
    assert_eq!(code, "conflict", "published on homepc: --take is needed");
    let t = s
        .ok(
            &laptop,
            None,
            "mailbox.publish",
            json!({"name": "desk", "take": true}),
        )
        .await;
    let g4 = g3 + 1;
    assert_eq!(
        unrevised(t),
        json!({"name": "desk", "mailbox_id": desk, "route_generation": g4})
    );
    // The old host is pushed its routes without the mailbox.
    assert_eq!(routes_of(&home.next("routes").await), []);
    let mut lap = s.v2(&laptop, "v2-laptop", 8, &[&desk]).await;
    assert_eq!(routes_of(&lap.next("routes").await), [(desk.clone(), g4)]);
    let again = lap.next("notify").await;
    assert_eq!(
        (
            again["message_id"].as_str(),
            again["binding_version"].as_u64()
        ),
        (Some(id.as_str()), Some(g4))
    );
    let old = s
        .attempts()
        .await
        .into_iter()
        .find(|a| a.id == n["attempt_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        old.status, "abandoned",
        "the old host's attempt at the old generation"
    );

    // homepc's unpublish, replayed after the move, does not unbind it.
    let (code, _) = s
        .err(
            &fx.kd,
            None,
            "mailbox.unpublish",
            json!({"mailbox_id": desk, "expected_generation": g3}),
        )
        .await;
    assert_eq!(code, "conflict");
    let row = s.agent("kd", "desk").await;
    assert_eq!(
        (row.address.unwrap().host, row.binding_version as u64),
        ("laptop".to_string(), g4)
    );
    assert_eq!(
        unrevised(s.ok(&fx.kd, None, "mailbox.mine", json!({})).await),
        json!({"mailboxes": []})
    );
}

/// The answer without its route revision (checked on its own below).
fn unrevised(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("revision");
    }
    v
}

/// A host's route revision moves with every binding change that touches the host, in the same
/// transaction, and rides every snapshot and answer: `routes`, `mailbox.mine`, `mailbox.publish`,
/// `mailbox.unpublish`. A takeover moves the old host's revision too. A daemon orders snapshots
/// by it, not by when their answers arrive.
async fn the_route_revision_orders_snapshots_and_answers(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    let rev = |v: &Value| {
        v["revision"]
            .as_u64()
            .unwrap_or_else(|| panic!("no revision in {v}"))
    };
    let r0 = rev(&home.next("routes").await);
    assert_eq!(
        rev(&s.ok(&fx.kd, None, "mailbox.mine", json!({})).await),
        r0
    );
    let g = s.agent("kd", "desk").await.binding_version as u64;

    let u = s
        .ok(
            &fx.kd,
            None,
            "mailbox.unpublish",
            json!({"mailbox_id": desk, "expected_generation": g}),
        )
        .await;
    let r1 = rev(&u);
    assert!(r1 > r0, "{r1} > {r0}");
    assert_eq!(rev(&home.next("routes").await), r1);

    let p = s
        .ok(&fx.kd, None, "mailbox.publish", json!({"name": "desk"}))
        .await;
    let r2 = rev(&p);
    assert!(r2 > r1);
    assert_eq!(
        rev(&s
            .ok(&fx.kd, None, "mailbox.publish", json!({"name": "desk"}))
            .await),
        r2,
        "a no-op moves nothing"
    );
    assert_eq!(
        rev(&s.ok(&fx.kd, None, "mailbox.mine", json!({})).await),
        r2
    );
    assert_eq!(rev(&home.next("routes").await), r2);

    let laptop = s.key("kd", "laptop").await;
    s.migrate(&laptop, &[], &conde::util::new_ulid()).await;
    let before = rev(&s.ok(&laptop, None, "mailbox.mine", json!({})).await);
    let t = s
        .ok(
            &laptop,
            None,
            "mailbox.publish",
            json!({"name": "desk", "take": true}),
        )
        .await;
    assert!(rev(&t) > before, "the new host's revision moved");
    let r3 = rev(&home.next("routes").await);
    assert!(
        r3 > r2,
        "the old host's revision moved with the takeover: {r3} > {r2}"
    );
    assert_eq!(
        rev(&s.ok(&fx.kd, None, "mailbox.mine", json!({})).await),
        r3
    );
}

/// The old host applied the routes frame and reports its attempt at the old generation refused
/// before the new host's bridge has claimed anything: the notification is not ended; the attempt
/// is abandoned and re-attempted at the current route.
async fn an_outcome_at_a_moved_generation_reattempts_at_the_current_route(d: Arc<Dispatcher>) {
    moved_generation(d, "refused", Some("recipient unavailable")).await;
}

/// A's delayed `delivered` for an attempt at the generation B took over does not complete the
/// notification; it stays for B (Astra's integrated review, finding 6).
async fn a_late_delivered_at_a_moved_generation_reattempts_at_the_current_route(
    d: Arc<Dispatcher>,
) {
    moved_generation(d, "delivered", None).await;
}

async fn moved_generation(d: Arc<Dispatcher>, status: &str, reason: Option<&str>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let n = home.next("notify").await; // the DM from before the migration, at the mailbox
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    let laptop = s.key("kd", "laptop").await;
    s.migrate(&laptop, &[], &conde::util::new_ulid()).await;
    s.ok(
        &laptop,
        None,
        "mailbox.publish",
        json!({"name": "desk", "take": true}),
    )
    .await;
    assert_eq!(routes_of(&home.next("routes").await), []);
    let ack = home.outcome(&attempt, 1, status, reason).await;
    assert_eq!(ack["rejected"], false, "{ack}");
    let old = s
        .attempts()
        .await
        .into_iter()
        .find(|a| a.id == attempt)
        .unwrap();
    assert_eq!(old.status, "abandoned", "{old:?}");
    let note = blocking({
        let d = d.clone();
        move || d.with_conn(|t| t.notifications()).unwrap()
    })
    .await
    .into_iter()
    .find(|x| n["notification_id"] == x.id.as_str())
    .unwrap();
    assert_eq!(
        note.status, "pending",
        "the notification waits for the current route"
    );
    let mut lap = s.v2(&laptop, "v2-laptop-moved", 8, &[&desk]).await;
    let again = lap.next("notify").await;
    assert_eq!(again["notification_id"], n["notification_id"]);
    assert_eq!(
        again["binding_version"].as_u64(),
        Some(s.agent("kd", "desk").await.binding_version as u64)
    );
}

// --- G11 (server half) ------------------------------------------------------------------------

async fn g11_the_window_bounds_claims_while_acks_and_routes_flow(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let desk = id(s, "desk").await;
    s.migrate(&fx.kd, &[&desk], "m1").await;
    let mut home = s.v2(&fx.kd, "v2-window", 2, &[&desk]).await;
    home.next("routes").await;
    for i in 0..4 {
        s.send(&fx.ben, RELAY, "kd/desk", &format!("dm {i}")).await;
    }
    // 5 due (one from before the migration): only the window's 2 are claimed and pushed.
    let a = home.next("notify").await;
    let b = home.next("notify").await;
    home.none("notify", Duration::from_millis(400)).await;
    let attempts = s.attempts().await;
    let dispatching = |v: &[conde::store::AttemptRow]| {
        v.iter()
            .filter(|x| x.bridge_instance == "v2-window" && x.status == "dispatching")
            .count()
    };
    assert_eq!(dispatching(&attempts), 2, "{attempts:?}");
    let pending = blocking({
        let d = d.clone();
        move || d.with_conn(|t| t.notifications()).unwrap()
    })
    .await
    .into_iter()
    .filter(|n| n.recipient_name == "kd/desk" && n.status == "pending")
    .count();
    assert_eq!(pending, 3, "the rest waits on the board");

    // While the window is full, routes frames and acks are still served.
    s.ok(&fx.kd, None, "mailbox.publish", json!({"name": "scribe"}))
        .await;
    assert_eq!(
        routes_of(&home.next("routes").await).len(),
        2,
        "desk, scribe"
    );
    let bad = home
        .outcome(
            a["attempt_id"].as_str().unwrap(),
            1,
            "failed",
            Some("cannot deliver to /run/x.sock"),
        )
        .await;
    assert_eq!(
        (bad["rejected"].clone(), bad["retry"].clone()),
        (json!(true), json!(false)),
        "{bad}"
    );
    home.none("notify", Duration::from_millis(300)).await;
    // A first outcome frees one slot: exactly one more is pushed.
    let ack = home
        .outcome(a["attempt_id"].as_str().unwrap(), 1, "held", None)
        .await;
    assert_eq!(ack["rejected"], false, "{ack}");
    let c = home.next("notify").await;
    home.none("notify", Duration::from_millis(300)).await;
    let ack = home
        .outcome(
            b["attempt_id"].as_str().unwrap(),
            1,
            "failed",
            Some("transport error"),
        )
        .await;
    assert_eq!(ack["rejected"], false, "{ack}");
    let d4 = home.next("notify").await;
    home.none("notify", Duration::from_millis(300)).await;

    // The same instance reconnects with a smaller window: every attempt it has seen and not
    // settled is offered again (held a, in flight c and d4), and nothing new is claimed.
    drop(home);
    let mut again = s.v2(&fx.kd, "v2-window", 1, &[&desk]).await;
    again.next("routes").await;
    let mut offered: Vec<String> = Vec::new();
    for _ in 0..3 {
        offered.push(
            again.next("notify").await["attempt_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    offered.sort();
    let mut expected: Vec<String> = [&a, &c, &d4]
        .iter()
        .map(|f| f["attempt_id"].as_str().unwrap().to_string())
        .collect();
    expected.sort();
    assert_eq!(offered, expected);
    again.none("notify", Duration::from_millis(400)).await;
    for f in [&c, &d4] {
        again
            .outcome(f["attempt_id"].as_str().unwrap(), 1, "delivered", None)
            .await;
    }
    // Freed: the last pending notification follows (b's retry waits for its backoff).
    let e = again.next("notify").await;
    assert!(
        ![&a, &b, &c, &d4]
            .iter()
            .any(|f| f["attempt_id"] == e["attempt_id"])
    );
    again.none("notify", Duration::from_millis(300)).await;
}

// --- refusals ---------------------------------------------------------------------------------

async fn a_migrated_host_refuses_session_addresses_and_v1_bridges(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    // A v2 bridge from a host that has not migrated is refused, as a final bad_hello.
    let mut early = s.ws(&fx.ben).await;
    early.send(json!({"v": 1, "type": "hello", "host_id": "benbox", "bridge_instance": "b2", "endpoints": [],
                      "proto": 2, "window": 4}))
        .await;
    let e = early.refused().await;
    assert_eq!(e["code"], "bad_hello", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("not migrated"),
        "{e}"
    );

    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = &ids[0];
    home.next("routes").await;
    let mut v1 = s.ws(&fx.kd).await;
    v1.send(json!({"v": 1, "type": "hello", "host_id": "homepc", "bridge_instance": "v1-late", "endpoints": []}))
        .await;
    let e = v1.refused().await;
    assert_eq!(e["code"], "bad_hello", "{e}");
    assert!(
        e["message"].as_str().unwrap().starts_with("upgrade conde"),
        "{e}"
    );

    let mb = mailbox(desk);
    for (actor, op, body) in [
        (
            DESK,
            "message.send",
            json!({"to": {"agent": "ben/relay"}, "text": "as a session"}),
        ),
        (WORKER, "agent.whoami", json!({})),
        (mb.as_str(), "agent.register", json!({"name": "newname"})),
        (KD, "agent.pin", json!({"name": "desk", "address": DESK})),
        (mb.as_str(), "agent.rename", json!({"new": "desk2"})),
    ] {
        let (code, message) = s.err(&fx.kd, Some(actor), op, body).await;
        assert_eq!(code, "forbidden", "{op} as {actor}: {message}");
    }
    // As its mailbox the host acts normally.
    let me = s.ok(&fx.kd, Some(&mb), "agent.whoami", json!({})).await;
    assert_eq!(me["name"], "kd/desk");
    s.send(&fx.kd, &mb, "ben/relay", "as the mailbox").await;

    // Bridge ops of the v2 instance: sessions are never endpoints, reasons are the closed set.
    let instance = blocking({
        let d = d.clone();
        move || d.with_conn(|t| t.active_bridges("homepc")).unwrap()
    })
    .await
    .remove(0);
    let p = blocking({
        let (d, instance) = (d.clone(), instance.clone());
        move || {
            let uid = d.with_conn(|t| t.user_id("kd")).unwrap().unwrap();
            d.remote_principal(uid, "homepc", Some(&instance)).unwrap()
        }
    })
    .await;
    let call = |op: &str, body: Value| {
        let (d, p) = (d.clone(), p.clone());
        let req = conde::protocol::Request::new("r", op, body);
        blocking(move || d.dispatch(&p, &req))
    };
    let r = call(
        "bridge.endpoints",
        json!({"endpoints": [{"kind": "claude", "target": "s-desk-5f2c"}]}),
    )
    .await;
    assert_eq!(
        r.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
    let r = call(
        "bridge.endpoints",
        json!({"endpoints": [{"kind": "mailbox", "target": desk}]}),
    )
    .await;
    assert!(r.ok, "{r:?}");
    let r = call("bridge.claim", json!({"limit": 10})).await;
    assert_eq!(
        r.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden),
        "a claim without a window"
    );
    let n = home.next("notify").await;
    let r = call(
        "bridge.report",
        json!({"attempt_id": n["attempt_id"], "outcome": "failed", "reason": "no live Claude session s-desk-5f2c"}),
    )
    .await;
    assert_eq!(
        r.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::BadRequest)
    );
}

async fn min_proto_2_refuses_every_v1_hello(d: Arc<Dispatcher>) {
    let fx = fixture(
        &d,
        ServerConfig {
            min_proto: 2,
            ..config()
        },
    )
    .await;
    let mut ws = fx.s.ws(&fx.ben).await;
    ws.send(json!({"v": 1, "type": "hello", "host_id": "benbox", "bridge_instance": "b1", "endpoints": []})).await;
    let e = ws.refused().await;
    assert_eq!(e["code"], "bad_hello", "{e}");
    assert!(
        e["message"].as_str().unwrap().starts_with("upgrade conde"),
        "{e}"
    );
    let (mut home, _) = migrated(&fx, &["desk"], 4).await;
    home.next("routes").await;
}

// --- listings, fleet, board.info, holds -------------------------------------------------------

async fn listings_show_kind_and_host_never_a_target(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let secrets = [
        "s-desk-5f2c",
        "t-worker-91aa",
        "s-lonely-77d0",
        "t-relay-3b1e",
    ];
    let check = |what: &str, v: &Value| {
        let text = v.to_string();
        for x in secrets {
            assert!(!text.contains(x), "{what} shows {x}: {text}");
        }
    };
    // Mixed board: v1 rows still carry harness bindings, and no listing shows them.
    let list = s.ok(&fx.ben, Some(RELAY), "agent.list", json!({})).await;
    check("agent.list", &list);
    let desk = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "kd/desk")
        .unwrap();
    assert_eq!(
        (desk["kind"].as_str(), desk["host"].as_str()),
        (Some("claude"), Some("homepc"))
    );
    check(
        "agent.resolve",
        &s.ok(
            &fx.ben,
            Some(RELAY),
            "agent.resolve",
            json!({"name": "kd/desk"}),
        )
        .await,
    );
    let prof = s
        .ok(
            &fx.ben,
            Some(RELAY),
            "profile.get",
            json!({"name": "kd/desk"}),
        )
        .await;
    check("profile.get", &prof);
    assert_eq!(prof["kind"], "claude");
    check(
        "own profile.get",
        &s.ok(&fx.kd, Some(DESK), "profile.get", json!({"name": "desk"}))
            .await,
    );
    check(
        "fleet.list",
        &s.ok(&fx.ben, Some(RELAY), "fleet.list", json!({})).await,
    );
    check(
        "own fleet.list",
        &s.ok(&fx.kd, Some(DESK), "fleet.list", json!({})).await,
    );
    let (_home, ids) = migrated(&fx, &["desk"], 4).await;
    let list = s.ok(&fx.ben, Some(RELAY), "agent.list", json!({})).await;
    check("agent.list after migration", &list);
    let desk = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "kd/desk")
        .unwrap();
    assert_eq!(
        (desk["kind"].as_str(), desk["host"].as_str()),
        (Some("mailbox"), Some("homepc"))
    );
    assert!(
        !list.to_string().contains(&ids[0]),
        "not even the mailbox id: {list}"
    );
}

async fn a_mailbox_row_is_reached_only_when_delivered(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, _) = migrated(&fx, &["desk"], 8).await;
    home.next("routes").await;
    let reached = |f: &Value| {
        let a = f["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "kd/desk")
            .unwrap();
        (a["ever_reached"].clone(), a["last_reached"].clone())
    };
    let n = home.next("notify").await; // pushed after the migration (a re-attempt at the mailbox)
    let attempt = n["attempt_id"].as_str().unwrap();
    home.outcome(attempt, 1, "accepted", None).await;
    let f = s.ok(&fx.kd, None, "fleet.list", json!({})).await;
    // Before the migration desk was a Claude row: no delivered attempt; accepted counts for codex rows only.
    assert_eq!(
        reached(&f),
        (json!(false), Value::Null),
        "accepted is not a reach for a mailbox"
    );
    home.outcome(attempt, 2, "delivered", None).await;
    let f = s.ok(&fx.kd, None, "fleet.list", json!({})).await;
    let (ever, last) = reached(&f);
    assert_eq!(ever, json!(true));
    assert!(last.is_string(), "{f}");
}

async fn board_info_names_the_board_account_and_host(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let info = fx.s.ok(&fx.kd, None, "board.info", json!({})).await;
    let default = json!({"admin_agent": null, "lobby": null, "guide": "https://conde.cloud/docs/setup",
                         "text": conde_remote::web::DEFAULT_WELCOME_TEXT});
    assert_eq!(
        info,
        json!({"name": "shared", "owner": "kd", "host": "homepc", "mailbox_only": false,
               "welcome": default})
    );
    // the workspace's welcome as the admin stored it, the defaults filled
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(|t| {
            Ok(t.set_setting("welcome", r##"{"admin_agent":"kd/desk","lobby":"#lobby"}"##)?)
        })
        .unwrap()
    })
    .await;
    let info = fx.s.ok(&fx.kd, None, "board.info", json!({})).await;
    assert_eq!(
        info["welcome"],
        json!({"admin_agent": "kd/desk", "lobby": "#lobby", "guide": "https://conde.cloud/docs/setup",
               "text": conde_remote::web::DEFAULT_WELCOME_TEXT})
    );
    let (_home, _) = migrated(&fx, &[], 4).await;
    let info = fx.s.ok(&fx.kd, None, "board.info", json!({})).await;
    assert_eq!(info["mailbox_only"], true);
    // A local board has no shared-board ops.
    let local = Dispatcher::open_in_memory().unwrap();
    let p = local.principal("kd", "box", true).unwrap();
    let r = local.dispatch(
        &p,
        &conde::protocol::Request::new("r", "board.info", json!({})),
    );
    assert_eq!(
        r.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
}

async fn a_held_mailbox_attempt_is_abandoned_when_the_mailbox_leaves_the_endpoints(
    d: Arc<Dispatcher>,
) {
    let fx = fixture(&d, config()).await;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let n = home.next("notify").await;
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    home.outcome(&attempt, 1, "held", None).await;
    home.send(json!({"v": 1, "type": "endpoints", "added": [], "removed": [{"kind": "mailbox", "mailbox_id": desk}]}))
        .await;
    let again = home.next("notify").await;
    assert_ne!(again["attempt_id"], attempt.as_str(), "re-attempted");
    let old =
        fx.s.attempts()
            .await
            .into_iter()
            .find(|a| a.id == attempt)
            .unwrap();
    assert_eq!(
        (old.status.as_str(), old.reason.as_deref()),
        ("abandoned", Some("recipient offline"))
    );
}

/// Whether the board lists `mailbox` among the host's live endpoints.
async fn endpoint_live(d: &Arc<Dispatcher>, mailbox: &str) -> bool {
    let (d, m) = (d.clone(), mailbox.to_string());
    blocking(move || d.with_conn(|t| t.endpoint_rows()).unwrap())
        .await
        .iter()
        .any(|e| e.target == m)
}

/// A hold that reaches the board after its mailbox left the endpoints (the removal was recorded
/// while the attempt was dispatching) is abandoned as the removal would have abandoned it.
async fn a_hold_reported_after_its_mailbox_left_is_abandoned(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let n = home.next("notify").await;
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    home.send(json!({"v": 1, "type": "endpoints", "added": [], "removed": [{"kind": "mailbox", "mailbox_id": desk}]}))
        .await;
    eventually("the removal recorded", || async {
        !endpoint_live(&d, &desk).await
    })
    .await;
    let a =
        fx.s.attempts()
            .await
            .into_iter()
            .find(|a| a.id == attempt)
            .unwrap();
    assert_eq!(
        a.status, "dispatching",
        "the removal leaves a dispatching attempt alone"
    );
    home.outcome(&attempt, 1, "held", None).await;
    let a =
        fx.s.attempts()
            .await
            .into_iter()
            .find(|a| a.id == attempt)
            .unwrap();
    assert_eq!(
        (a.status.as_str(), a.reason.as_deref()),
        ("abandoned", Some("recipient offline"))
    );
}

/// A mailbox that leaves and comes back, then a hold, all sent back to back: the board records
/// them in that order, so the hold finds its mailbox live and is kept.
async fn a_hold_reported_after_its_mailbox_came_back_is_kept(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let n = home.next("notify").await;
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    let mb = json!([{"kind": "mailbox", "mailbox_id": desk}]);
    home.send(json!({"v": 1, "type": "endpoints", "added": [], "removed": mb}))
        .await;
    home.send(json!({"v": 1, "type": "endpoints", "added": mb, "removed": []}))
        .await;
    home.outcome(&attempt, 1, "held", None).await;
    let a =
        fx.s.attempts()
            .await
            .into_iter()
            .find(|a| a.id == attempt)
            .unwrap();
    assert_eq!((a.status.as_str(), a.reason.as_deref()), ("held", None));
    assert!(endpoint_live(&d, &desk).await);
}

/// A new bridge instance of the host (a daemon restart) abandons and re-offers only the old
/// instance's attempts whose notification is still open. A delivered notification keeps its
/// delivered attempt; one the recipient already read keeps its accepted or held attempt and
/// is settled as read, never re-attempted.
async fn a_takeover_abandons_only_open_attempts(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let me = mailbox(&desk);
    let read = |id: Value| s.ok(&fx.kd, Some(me.as_str()), "message.read", json!({"id": id}));
    // Delivered, then read.
    let delivered = home.next("notify").await; // the DM from before the migration
    home.outcome(
        delivered["attempt_id"].as_str().unwrap(),
        1,
        "delivered",
        None,
    )
    .await;
    read(delivered["message_id"].clone()).await;
    // Accepted without a receipt, then read.
    let id = s.send(&fx.ben, RELAY, "kd/desk", "accepted and read").await;
    let accepted = home.notify_for(&id).await;
    home.outcome(
        accepted["attempt_id"].as_str().unwrap(),
        1,
        "accepted",
        None,
    )
    .await;
    read(json!(id)).await;
    // Held, then read.
    let id = s.send(&fx.ben, RELAY, "kd/desk", "held and read").await;
    let held = home.notify_for(&id).await;
    home.outcome(held["attempt_id"].as_str().unwrap(), 1, "held", None)
        .await;
    read(json!(id)).await;
    // Accepted, not read: still open.
    let id = s.send(&fx.ben, RELAY, "kd/desk", "still open").await;
    let open = home.notify_for(&id).await;
    home.outcome(open["attempt_id"].as_str().unwrap(), 1, "accepted", None)
        .await;

    let mut next = s.v2(&fx.kd, "v2-after-restart", 8, &[&desk]).await;
    next.next("routes").await;
    let again = next.next("notify").await;
    assert_eq!(
        again["notification_id"], open["notification_id"],
        "only the open one is re-offered: {again}"
    );
    assert_ne!(again["attempt_id"], open["attempt_id"]);
    next.none("notify", Duration::from_millis(300)).await;

    let attempts = s.attempts().await;
    let attempt = |n: &Value| {
        let a = attempts
            .iter()
            .find(|a| n["attempt_id"] == a.id.as_str())
            .unwrap();
        (a.status.clone(), a.reason.clone())
    };
    assert_eq!(attempt(&delivered), ("delivered".into(), None));
    assert_eq!(attempt(&accepted), ("accepted".into(), None));
    assert_eq!(attempt(&held), ("held".into(), None));
    assert_eq!(
        attempt(&open),
        ("abandoned".into(), Some("bridge instance gone".into()))
    );
    let notes = blocking({
        let d = d.clone();
        move || d.with_conn(|t| t.notifications()).unwrap()
    })
    .await;
    let status = |n: &Value| {
        notes
            .iter()
            .find(|x| n["notification_id"] == x.id.as_str())
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(
        [
            status(&delivered),
            status(&accepted),
            status(&held),
            status(&open)
        ],
        ["delivered", "read", "read", "dispatching"]
    );
}

/// The notification of `n` (a notify frame), as the board holds it.
async fn note_of(d: &Arc<Dispatcher>, n: &Value) -> String {
    let d = d.clone();
    let all = blocking(move || d.with_conn(|t| t.notifications()).unwrap()).await;
    all.into_iter()
        .find(|x| n["notification_id"] == x.id.as_str())
        .unwrap()
        .status
}

/// A hold whose message the recipient has read is not abandoned when its mailbox leaves the
/// endpoints: the notification settles as read, the attempt stays held, and it is never
/// offered again, not even to the same instance reconnecting.
async fn a_read_hold_is_kept_when_the_mailbox_leaves_the_endpoints(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let desk = id(s, "desk").await;
    s.migrate(&fx.kd, &[&desk], "m-read-hold").await;
    let mut home = s.v2(&fx.kd, "v2-read-hold", 8, &[&desk]).await;
    home.next("routes").await;
    let n = home.next("notify").await;
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    home.outcome(&attempt, 1, "held", None).await;
    s.ok(
        &fx.kd,
        Some(mailbox(&desk).as_str()),
        "message.read",
        json!({"id": n["message_id"]}),
    )
    .await;
    home.send(json!({"v": 1, "type": "endpoints", "added": [], "removed": [{"kind": "mailbox", "mailbox_id": desk}]}))
        .await;
    eventually("the removal recorded", || async {
        !endpoint_live(&d, &desk).await
    })
    .await;
    home.none("notify", Duration::from_millis(300)).await;
    let a = s
        .attempts()
        .await
        .into_iter()
        .find(|a| a.id == attempt)
        .unwrap();
    assert_eq!(
        (a.status.as_str(), a.reason.as_deref()),
        ("held", None),
        "the attempt is left as it is"
    );
    assert_eq!(note_of(&d, &n).await, "read");
    drop(home);
    let mut again = s.v2(&fx.kd, "v2-read-hold", 8, &[&desk]).await;
    again.next("routes").await;
    again.none("notify", Duration::from_millis(300)).await;
}

/// An outcome A reports after B took over the mailbox, for a message the recipient has read
/// meanwhile: the notification settles as read, the attempt is left as it is and nothing is
/// re-attempted at B.
async fn a_late_outcome_at_a_moved_generation_keeps_a_read_notification_read(d: Arc<Dispatcher>) {
    let fx = fixture(&d, config()).await;
    let s = &fx.s;
    let (mut home, ids) = migrated(&fx, &["desk"], 8).await;
    let desk = ids[0].clone();
    home.next("routes").await;
    let n = home.next("notify").await;
    let attempt = n["attempt_id"].as_str().unwrap().to_string();
    s.ok(
        &fx.kd,
        Some(mailbox(&desk).as_str()),
        "message.read",
        json!({"id": n["message_id"]}),
    )
    .await;
    let laptop = s.key("kd", "laptop").await;
    s.migrate(&laptop, &[], &conde::util::new_ulid()).await;
    s.ok(
        &laptop,
        None,
        "mailbox.publish",
        json!({"name": "desk", "take": true}),
    )
    .await;
    assert_eq!(routes_of(&home.next("routes").await), []);
    let ack = home
        .outcome(&attempt, 1, "failed", Some("transport error"))
        .await;
    assert_eq!(ack["rejected"], false, "{ack}");
    let a = s
        .attempts()
        .await
        .into_iter()
        .find(|a| a.id == attempt)
        .unwrap();
    assert_eq!(
        (a.status.as_str(), a.reason.as_deref()),
        ("dispatching", None),
        "the attempt is left as it is"
    );
    assert_eq!(note_of(&d, &n).await, "read");
    let mut lap = s.v2(&laptop, "v2-laptop-read", 8, &[&desk]).await;
    lap.next("routes").await;
    lap.none("notify", Duration::from_millis(300)).await;
}

// --- the SQLite file of a v1 board ------------------------------------------------------------

#[test]
fn an_old_sqlite_board_is_rebuilt_to_accept_mailboxes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde.db");
    let old = include_str!("../src/store/sqlite_schema.sql")
        .replace("'human', 'mailbox')", "'human')")
        .replace(",\n    mailbox_only INTEGER NOT NULL DEFAULT 0\n", "\n");
    assert!(!old.contains("mailbox_only") && !old.contains("'mailbox'"));
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(&old).unwrap();
        c.execute_batch(
            "INSERT INTO users(name) VALUES ('kd');
             INSERT INTO hosts(host_id, user_id) VALUES ('homepc', 1);
             INSERT INTO agents(agent_id, owner_id, name_lower, address_kind, address_target, host_id, binding_version,
                                pinned_address)
                 VALUES ('A1', 1, 'desk', 'claude', 's1', 'homepc', 3, 'claude:s1@homepc');
             INSERT INTO agents(agent_id, owner_id, name_lower) VALUES ('A2', 1, 'other');
             INSERT INTO messages(message_id, sender_agent_id, recipient_agent_id, text, created_at)
                 VALUES ('M1', 'A1', 'A2', 'hi', '2026-10-09T00:00:00.000Z');",
        )
        .unwrap();
        let refused = c.execute(
            "UPDATE agents SET address_kind = 'mailbox' WHERE agent_id = 'A1'",
            [],
        );
        assert!(refused.is_err(), "the old CHECK refuses a mailbox");
    }
    let d = Dispatcher::open(&path).unwrap();
    let a = d.with_conn(|t| t.agent_by_id("A1")).unwrap().unwrap();
    assert_eq!(
        (a.binding_version, a.pinned.as_deref()),
        (3, Some("claude:s1@homepc")),
        "rows kept"
    );
    d.with_tx(|t| {
        t.set_binding(
            "A1",
            Some(&conde::address::Address::parse("mailbox:A1@homepc", "homepc").unwrap()),
        )?;
        t.set_host_mailbox_only("homepc")?;
        Ok(())
    })
    .unwrap();
    let a = d.with_conn(|t| t.agent_by_id("A1")).unwrap().unwrap();
    assert_eq!(a.address.unwrap().to_string(), "mailbox:A1@homepc");
    assert!(d.with_conn(|t| t.host_mailbox_only("homepc")).unwrap());
    assert_eq!(
        d.with_conn(|t| t.message_by_id("M1"))
            .unwrap()
            .unwrap()
            .sender,
        "kd/desk"
    );
    drop(d);
    let c = rusqlite::Connection::open(&path).unwrap();
    assert!(
        !c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let indexes: i64 = c
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'agents_by_address'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(indexes, 1);
    // Opening again leaves the rebuilt table alone.
    drop(c);
    Dispatcher::open(&path).unwrap();
}
