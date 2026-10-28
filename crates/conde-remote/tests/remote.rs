//! Offline tests on 127.0.0.1 with fake core implementations.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conde_remote::{
    Authenticator, BridgeClient, BridgeConfig, BridgeState, CallError, Dispatcher, Endpoint, FenceResult,
    NotificationSource, Notify, Outcome, Principal, Recorded, ServerConfig, ServerHandle,
    StopReason,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

const WAIT: Duration = Duration::from_secs(5);

struct FakeAuth;

impl Authenticator for FakeAuth {
    fn authenticate(&self, token: &str) -> Option<Principal> {
        let (user, host) = match token {
            "tok-a" => ("alice", "host-a"),
            "tok-b" => ("bob", "host-b"),
            _ => return None,
        };
        Some(Principal {
            user_id: user.into(),
            host_id: host.into(),
        })
    }
}

/// Echoes the op and the principal it was called with; the body's claims are ignored.
struct FakeDispatcher;

impl Dispatcher for FakeDispatcher {
    fn dispatch(&self, p: &Principal, request: Value) -> Value {
        json!({
            "v": 1, "type": "response", "request_id": "overwritten-by-server", "ok": true,
            "result": {"op": request["op"], "user": p.user_id, "host": p.host_id},
        })
    }
}

#[derive(Default)]
struct State {
    pending: Vec<(String, Notify)>,
    /// Outcomes for which record_outcome returned true.
    recorded: Vec<Outcome>,
    record_calls: Vec<(String, u64)>,
    active: HashMap<String, String>,
    fenced: HashSet<String>,
    fence_calls: Vec<(String, String)>,
    endpoints: Vec<Endpoint>,
    endpoint_updates: Vec<Vec<Endpoint>>,
    /// Heartbeat writes (each takes a moment, like a store write).
    heartbeats: usize,
    /// (host, instance) of the connection each outcome arrived on.
    outcome_conns: Vec<(String, String)>,
    /// The next this-many record_outcome calls answer Retry (a transient store failure).
    retry_next: usize,
    /// A Retry answer still records the outcome first (the ack was lost, not the write).
    retry_after_record: bool,
    /// Outcome and endpoint writes in the order they reached the store.
    writes: Vec<String>,
    /// How long an outcome for a "slow-" attempt takes to record.
    slow_outcome: Duration,
    /// The next this-many endpoints writes answer Retry.
    endpoints_fail_next: usize,
    /// A snapshot holding a session "slow" waits until this is cleared.
    hold_slow_endpoints: bool,
    /// Set while such a snapshot is being written.
    slow_endpoints_writing: bool,
}

#[derive(Default)]
struct FakeSource(Mutex<State>);

impl FakeSource {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
    fn add(&self, host: &str, attempt: &str) {
        self.state().pending.push((host.into(), notify(attempt)));
    }
    fn record_calls_for(&self, attempt: &str) -> usize {
        self.state()
            .record_calls
            .iter()
            .filter(|(a, _)| a == attempt)
            .count()
    }
}

impl NotificationSource for FakeSource {
    fn pending_for_host(&self, host_id: &str, _bridge_instance: &str) -> Vec<Notify> {
        let s = self.state();
        s.pending
            .iter()
            .filter(|(h, n)| {
                h == host_id && !s.recorded.iter().any(|o| o.attempt_id == n.attempt_id)
            })
            .map(|(_, n)| n.clone())
            .collect()
    }

    fn record_outcome(&self, host_id: &str, instance: &str, outcome: &Outcome) -> Recorded {
        if outcome.attempt_id.starts_with("slow-") {
            let pause = self.state().slow_outcome;
            std::thread::sleep(pause);
        }
        let mut s = self.state();
        s.writes.push(format!("outcome {}", outcome.attempt_id));
        s.outcome_conns.push((host_id.into(), instance.into()));
        if outcome.attempt_id.starts_with("foreign-") {
            return Recorded::Rejected("attempt belongs to another bridge".into());
        }
        s.record_calls
            .push((outcome.attempt_id.clone(), outcome.event_seq));
        let dup = s
            .recorded
            .iter()
            .any(|o| o.attempt_id == outcome.attempt_id && o.event_seq == outcome.event_seq);
        if s.retry_next > 0 {
            s.retry_next -= 1;
            if s.retry_after_record && !dup {
                s.recorded.push(outcome.clone());
            }
            return Recorded::Retry("store busy".into());
        }
        if !dup {
            s.recorded.push(outcome.clone());
        }
        Recorded::Accepted
    }

    fn fence(&self, host_id: &str, bridge_instance: &str) -> FenceResult {
        let mut s = self.state();
        s.fence_calls.push((host_id.into(), bridge_instance.into()));
        if s.fenced.contains(bridge_instance) {
            return FenceResult::Fenced;
        }
        if let Some(prev) = s.active.insert(host_id.into(), bridge_instance.into())
            && prev != bridge_instance
        {
            s.fenced.insert(prev);
        }
        FenceResult::Active
    }

    fn endpoints(&self, _host_id: &str, _bridge_instance: &str, endpoints: &[Endpoint]) -> Recorded {
        let mut sorted = endpoints.to_vec();
        sorted.sort_by_key(|e| format!("{e:?}"));
        if sorted.contains(&claude("slow")) {
            self.state().slow_endpoints_writing = true;
            while self.state().hold_slow_endpoints {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let mut s = self.state();
        if s.endpoints_fail_next > 0 {
            s.endpoints_fail_next -= 1;
            return Recorded::Retry("store busy".into());
        }
        let names: Vec<String> = sorted
            .iter()
            .map(|e| match e {
                Endpoint::Claude { session_id } => session_id.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        s.writes.push(format!("endpoints [{}]", names.join(",")));
        s.endpoint_updates.push(sorted.clone());
        s.endpoints = sorted;
        Recorded::Accepted
    }

    fn heartbeat(&self, _host_id: &str, _bridge_instance: &str) {
        std::thread::sleep(Duration::from_millis(50));
        self.state().heartbeats += 1;
    }
}

fn claude(session: &str) -> Endpoint {
    Endpoint::Claude { session_id: session.into() }
}

fn notify(attempt: &str) -> Notify {
    Notify {
        notification_id: format!("n-{attempt}"),
        attempt_id: attempt.into(),
        recipient: "alice/agent.one".into(),
        binding_version: 3,
        endpoint: Endpoint::Claude {
            session_id: "sess-1".into(),
        },
        message_id: format!("m-{attempt}"),
        text: format!("hello {attempt}"),
        fields: None,
    }
}

fn test_config() -> ServerConfig {
    ServerConfig {
        poll_interval: Duration::from_millis(50),
        ..ServerConfig::default()
    }
}

async fn start_server(
    addr: SocketAddr,
    config: ServerConfig,
    source: Arc<FakeSource>,
) -> ServerHandle {
    conde_remote::start(
        addr,
        config,
        Arc::new(FakeAuth),
        Arc::new(FakeDispatcher),
        source,
    )
    .await
    .expect("server starts")
}

fn client_config(addr: SocketAddr, token: &str, host: &str, instance: &str) -> BridgeConfig {
    let mut c = BridgeConfig::new(format!("ws://{addr}/v1/bridge"), token, host);
    c.bridge_instance = Some(instance.into());
    c.backoff_min = Duration::from_millis(50);
    c.backoff_max = Duration::from_millis(200);
    c.endpoints = vec![Endpoint::Codex {
        thread_id: "th-1".into(),
    }];
    c
}

/// A client whose notifications land in the returned channel.
fn start_client(config: BridgeConfig) -> (BridgeClient, std::sync::mpsc::Receiver<Notify>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let client = BridgeClient::start(config, move |n| {
        let _ = tx.send(n);
    });
    (client, rx)
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_connected(client: &BridgeClient) {
    let mut rx = client.subscribe();
    tokio::time::timeout(WAIT, rx.wait_for(|s| *s == BridgeState::Connected))
        .await
        .expect("client connects")
        .expect("state channel open");
}

async fn recv_notify(rx: &std::sync::mpsc::Receiver<Notify>) -> Notify {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Ok(n) = rx.try_recv() {
            return n;
        }
        assert!(tokio::time::Instant::now() < deadline, "no notify arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Minimal HTTP/1.1 client: returns (status, body).
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let status = raw[9..12].parse().unwrap();
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

type RawWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn raw_ws(addr: SocketAddr, token: &str) -> Result<RawWs, tungstenite::Error> {
    let mut req = format!("ws://{addr}/v1/bridge")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn send_json(ws: &mut RawWs, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

/// The close frame's (code, reason), skipping text frames; None when the stream ends without one.
async fn recv_close(ws: &mut RawWs) -> Option<(u16, String)> {
    loop {
        match tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("close within timeout")
        {
            Some(Ok(Message::Close(Some(f)))) => return Some((f.code.into(), f.reason.to_string())),
            Some(Ok(Message::Close(None))) | None | Some(Err(_)) => return None,
            Some(Ok(_)) => continue,
        }
    }
}

fn outcome_frame(attempt: &str) -> Value {
    json!({"v":1,"type":"outcome","attempt_id":attempt,"event_seq":1,"status":"held","reason":null})
}

fn endpoints_frame(added: &[&str], removed: &[&str]) -> Value {
    let list = |s: &[&str]| s.iter().map(|id| json!({"kind":"claude","session_id":id})).collect::<Vec<_>>();
    json!({"v":1,"type":"endpoints","added":list(added),"removed":list(removed)})
}

/// Next text frame as JSON; None when the connection ends first.
async fn recv_json(ws: &mut RawWs) -> Option<Value> {
    loop {
        match tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("frame within timeout")
        {
            Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(&t).unwrap()),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
            Some(Ok(_)) => continue,
        }
    }
}

fn hello(host: &str, instance: &str) -> Value {
    json!({"v":1,"type":"hello","host_id":host,"bridge_instance":instance,"endpoints":[]})
}

fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn healthz_and_bad_tokens_are_401() {
    let server = start_server(local(), test_config(), Arc::default()).await;
    let addr = server.local_addr();

    assert_eq!(
        http(addr, "GET", "/healthz", None, "").await,
        (200, "ok".into())
    );
    assert_eq!(http(addr, "POST", "/v1/rpc", None, "{}").await.0, 401);
    assert_eq!(
        http(addr, "POST", "/v1/rpc", Some("nope"), "{}").await.0,
        401
    );

    match raw_ws(addr, "nope").await {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 401),
        other => panic!("expected a 401 handshake, got {:?}", other.map(|_| ())),
    }

    let (client, _rx) = start_client(client_config(addr, "nope", "host-a", "i-bad"));
    let reason = tokio::time::timeout(WAIT, client.stopped()).await.unwrap();
    assert_eq!(reason, StopReason::Unauthorized);
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_round_trip_stamps_principal() {
    let server = start_server(local(), test_config(), Arc::default()).await;
    let addr = server.local_addr();
    let request = json!({
        "v":1,"type":"request","request_id":"r1","op":"agent.whoami","idempotency_key":null,
        "actor":"agent.one","body":{"host_id":"host-evil","user_id":"mallory"},
    });
    let (status, body) = http(addr, "POST", "/v1/rpc", Some("tok-a"), &request.to_string()).await;
    assert_eq!(status, 200);
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resp["request_id"], "r1");
    assert_eq!(resp["ok"], true);
    assert_eq!(
        resp["result"],
        json!({"op":"agent.whoami","user":"alice","host":"host-a"})
    );

    let (status, body) = http(addr, "POST", "/v1/rpc", Some("tok-a"), "not json").await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "bad_request"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_with_foreign_host_is_rejected() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    let mut ws = raw_ws(addr, "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-b", "i-1")).await;
    let err = recv_json(&mut ws).await.expect("error frame");
    assert_eq!(
        (err["type"].as_str(), err["code"].as_str()),
        (Some("error"), Some("host_mismatch"))
    );
    assert_eq!(
        recv_json(&mut ws).await,
        None,
        "connection closed after the error"
    );
    assert!(
        source.state().fence_calls.is_empty(),
        "no fence for a rejected hello"
    );

    let (client, _rx) = start_client(client_config(addr, "tok-a", "host-b", "i-2"));
    let reason = tokio::time::timeout(WAIT, client.stopped()).await.unwrap();
    assert!(
        matches!(reason, StopReason::Rejected { ref code, .. } if code == "host_mismatch"),
        "{reason:?}"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hello_breaking_its_protocol_shape_is_refused() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();
    let mailbox = json!([{"kind": "mailbox", "mailbox_id": "01MBX"}]);
    let codex = json!([{"kind": "codex", "thread_id": "th-1"}]);
    let bad = [
        json!({"endpoints": mailbox}),
        json!({"window": 32}),
        json!({"proto": 2, "endpoints": mailbox}),
        json!({"proto": 2, "window": 32, "endpoints": codex}),
        json!({"proto": 2, "window": 0}),
        json!({"proto": 3, "window": 32}),
    ];
    for extra in bad {
        let mut frame = hello("host-a", "i-1");
        frame.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let mut ws = raw_ws(addr, "tok-a").await.unwrap();
        send_json(&mut ws, frame).await;
        let err = recv_json(&mut ws).await.expect("error frame");
        assert_eq!((err["type"].as_str(), err["code"].as_str()), (Some("error"), Some("bad_hello")), "{extra}");
        assert_eq!(recv_json(&mut ws).await, None, "connection closed after the error");
    }
    assert!(source.state().fence_calls.is_empty(), "no fence for a refused hello");
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn second_bridge_fences_the_first() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    let (first, _rx1) = start_client(client_config(addr, "tok-a", "host-a", "i-old"));
    // Connected means the handshake is done; the hello is fenced a moment later.
    wait_until("first fenced in", || source.state().fence_calls.len() == 1).await;
    let (second, _rx2) = start_client(client_config(addr, "tok-a", "host-a", "i-new"));
    wait_connected(&second).await;

    let reason = tokio::time::timeout(WAIT, first.stopped())
        .await
        .expect("first is fenced");
    assert_eq!(reason, StopReason::Fenced);
    let old_fences = source
        .state()
        .fence_calls
        .iter()
        .filter(|(_, i)| i == "i-old")
        .count();
    assert_eq!(
        old_fences, 1,
        "fenced on its live connection, not on a reconnect"
    );
    assert_eq!(second.state(), BridgeState::Connected);
    let resp = second.call(json!({"op":"doctor"})).await.unwrap();
    assert_eq!(resp["result"]["host"], "host-a");

    // The fenced instance may not come back.
    let mut ws = raw_ws(addr, "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-a", "i-old")).await;
    assert_eq!(recv_json(&mut ws).await.unwrap()["code"], "fenced");
    assert_eq!(second.state(), BridgeState::Connected);
    assert_eq!(
        source.state().endpoints,
        vec![Endpoint::Codex {
            thread_id: "th-1".into()
        }]
    );
    second.shutdown();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_outcome_ack_round_trip() {
    let source = Arc::new(FakeSource::default());
    source.add("host-a", "att-1");
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    let (client, rx) = start_client(client_config(addr, "tok-a", "host-a", "i-1"));
    assert_eq!(recv_notify(&rx).await, notify("att-1"));
    let held = client
        .report("att-1", "held", Some("permission parity"))
        .unwrap();
    let delivered = client.report("att-1", "delivered", None).unwrap();
    assert!(delivered > held, "event_seq increases per attempt");
    wait_until("outcomes acked", || client.unacked_outcomes() == 0).await;
    let recorded: Vec<_> = source
        .state()
        .recorded
        .iter()
        .map(|o| (o.event_seq, o.status.clone()))
        .collect();
    assert_eq!(
        recorded,
        vec![(held, "held".into()), (delivered, "delivered".into())]
    );

    // A notification added later is pushed by the poll.
    source.add("host-a", "att-2");
    assert_eq!(recv_notify(&rx).await.attempt_id, "att-2");
    client.shutdown();

    // Duplicate outcome on the wire: both acked, recorded once.
    source.add("host-b", "att-b");
    let mut ws = raw_ws(addr, "tok-b").await.unwrap();
    send_json(&mut ws, hello("host-b", "i-b")).await;
    let n = recv_json(&mut ws).await.unwrap();
    assert_eq!(
        (n["type"].as_str(), n["attempt_id"].as_str()),
        (Some("notify"), Some("att-b"))
    );
    assert_eq!(
        n["endpoint"],
        json!({"kind":"claude","session_id":"sess-1"})
    );
    let outcome = json!({"v":1,"type":"outcome","attempt_id":"att-b","event_seq":1,"status":"delivered","reason":null});
    send_json(&mut ws, outcome.clone()).await;
    send_json(&mut ws, outcome).await;
    for _ in 0..2 {
        let ack = recv_json(&mut ws).await.unwrap();
        assert_eq!(
            ack,
            json!({"v":1,"type":"outcome_ack","attempt_id":"att-b","event_seq":1,"rejected":false,"retry":false,"reason":null})
        );
    }
    source.state().retry_next = 1;
    send_json(&mut ws, json!({"v":1,"type":"outcome","attempt_id":"att-b","event_seq":2,"status":"delivered","reason":null})).await;
    assert_eq!(
        recv_json(&mut ws).await.unwrap(),
        json!({"v":1,"type":"outcome_ack","attempt_id":"att-b","event_seq":2,"rejected":false,"retry":true,"reason":"store busy"})
    );
    assert_eq!(source.record_calls_for("att-b"), 3);
    assert_eq!(
        source
            .state()
            .recorded
            .iter()
            .filter(|o| o.attempt_id == "att-b")
            .count(),
        1
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn server_resends_pending_to_a_reconnecting_instance() {
    let source = Arc::new(FakeSource::default());
    source.add("host-a", "att-1");
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    for round in 0..2 {
        let mut ws = raw_ws(addr, "tok-a").await.unwrap();
        send_json(&mut ws, hello("host-a", "i-1")).await;
        let n = recv_json(&mut ws).await.unwrap();
        assert_eq!(n["attempt_id"], "att-1", "round {round}");
        ws.close(None).await.unwrap();
    }
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn same_instance_reconnect_resends_only_unacked_outcomes() {
    let source = Arc::new(FakeSource::default());
    for a in ["att-a", "att-b", "att-c"] {
        source.add("host-a", a);
    }
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    let (client, rx) = start_client(client_config(addr, "tok-a", "host-a", "i-1"));
    let mut got = vec![];
    for _ in 0..3 {
        got.push(recv_notify(&rx).await.attempt_id);
    }
    assert_eq!(got, ["att-a", "att-b", "att-c"]);
    client.report("att-a", "delivered", None).unwrap();
    wait_until("att-a acked", || client.unacked_outcomes() == 0).await;

    server.shutdown().await.unwrap();
    let mut states = client.subscribe();
    tokio::time::timeout(WAIT, states.wait_for(|s| *s != BridgeState::Connected))
        .await
        .unwrap()
        .unwrap();
    client.report("att-b", "delivered", None).unwrap();
    assert_eq!(client.unacked_outcomes(), 1);

    let server = start_server(addr, test_config(), source.clone()).await;
    wait_until("att-b acked after reconnect", || {
        client.unacked_outcomes() == 0
    })
    .await;
    // att-c is still pending, so the new server pushes it again; the client must not
    // hand it to the harness twice.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rx.try_recv().is_err(),
        "a resent notify was delivered twice"
    );

    assert_eq!(
        source.record_calls_for("att-a"),
        1,
        "acked outcome re-emitted"
    );
    assert_eq!(source.record_calls_for("att-b"), 1);
    let fences: Vec<_> = source
        .state()
        .fence_calls
        .iter()
        .map(|(_, i)| i.clone())
        .collect();
    assert_eq!(
        fences,
        ["i-1", "i-1"],
        "same bridge_instance across reconnects"
    );
    assert_eq!(client.state(), BridgeState::Connected);
    client.shutdown();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_are_forwarded_over_the_bridge() {
    let server = start_server(local(), test_config(), Arc::default()).await;
    let addr = server.local_addr();
    let (client, _rx) = start_client(client_config(addr, "tok-b", "host-b", "i-1"));

    let resp = client
        .call(
            json!({"request_id":"q1","op":"message.send","actor":"agent.two","body":{"text":"hi"}}),
        )
        .await
        .unwrap();
    assert_eq!(resp["type"], "response");
    assert_eq!(resp["request_id"], "q1");
    assert_eq!(
        resp["result"],
        json!({"op":"message.send","user":"bob","host":"host-b"})
    );

    // Concurrent calls without ids each get their own response.
    let calls = (0..5).map(|i| {
        let c = client.clone();
        async move { c.call(json!({"op": format!("op.{i}")})).await.unwrap() }
    });
    let responses = futures_util::future::join_all(calls).await;
    for (i, r) in responses.iter().enumerate() {
        assert_eq!(r["result"]["op"], format!("op.{i}"));
    }
    client.shutdown();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pings_keep_an_idle_connection_alive_and_silent_peers_are_cut() {
    let source = Arc::new(FakeSource::default());
    let config = ServerConfig {
        ping_interval: Duration::from_millis(200),
        max_missed_pongs: 2,
        ..test_config()
    };
    let server = start_server(local(), config, source.clone()).await;
    let addr = server.local_addr();

    let mut cc = client_config(addr, "tok-a", "host-a", "i-1");
    cc.idle_timeout = Duration::from_secs(1);
    let (client, _rx) = start_client(cc);
    wait_connected(&client).await;

    // A peer that never reads never answers pings.
    let mut silent = raw_ws(addr, "tok-b").await.unwrap();
    send_json(&mut silent, hello("host-b", "i-silent")).await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(client.state(), BridgeState::Connected);
    assert_eq!(
        source.state().fence_calls.len(),
        2,
        "the pinged client never reconnected"
    );
    assert_eq!(
        client.call(json!({"op":"doctor"})).await.unwrap()["ok"],
        true
    );

    // The silent peer was closed after two missed pongs: its stream ends.
    let ended = tokio::time::timeout(Duration::from_millis(500), async {
        while let Some(Ok(_)) = silent.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "silent peer still connected");
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: unsolicited pongs write no heartbeat, and a burst answering one ping writes
/// exactly one.
#[tokio::test(flavor = "multi_thread")]
async fn only_an_answered_ping_writes_a_heartbeat_once() {
    let source = Arc::new(FakeSource::default());
    let config = ServerConfig {
        ping_interval: Duration::from_millis(300),
        max_missed_pongs: 5,
        ..test_config()
    };
    let server = start_server(local(), config, source.clone()).await;
    let mut ws = raw_ws(server.local_addr(), "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-a", "i-1")).await;
    for _ in 0..50 {
        ws.send(Message::Pong(Default::default())).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(source.state().heartbeats, 0, "no ping was outstanding");

    // The first ping: answered by a burst of pongs (the reader's automatic one included).
    loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("a ping").unwrap().unwrap() {
            Message::Ping(_) => break,
            _ => continue,
        }
    }
    for _ in 0..50 {
        ws.send(Message::Pong(Default::default())).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(source.state().heartbeats, 1, "one write for one answered ping");
    server.shutdown().await.unwrap();
}

/// Review fixture: a fenced client must not run notifications still queued for its callback;
/// the callback already running completes and its report is refused.
#[tokio::test(flavor = "multi_thread")]
async fn queued_callbacks_are_dropped_once_fenced() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let source = Arc::new(FakeSource::default());
    source.add("host-a", "a");
    source.add("host-a", "b");
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();

    let count = Arc::new(AtomicUsize::new(0));
    let slot: Arc<Mutex<Option<BridgeClient>>> = Arc::default();
    let first_report = Arc::new(Mutex::new(None));
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (c, s, fr) = (count.clone(), slot.clone(), first_report.clone());
    let old = BridgeClient::start(client_config(addr, "tok-a", "host-a", "old"), move |n| {
        if c.fetch_add(1, Ordering::SeqCst) == 0 {
            gate.recv().unwrap();
            let client = s.lock().unwrap().clone().unwrap();
            *fr.lock().unwrap() = Some(client.report(&n.attempt_id, "delivered", None));
        }
    });
    *slot.lock().unwrap() = Some(old.clone());
    wait_until("first callback", || count.load(Ordering::SeqCst) == 1).await;
    // The round trip proves notify b already sits in the callback queue.
    old.call(json!({"op":"barrier"})).await.unwrap();

    let (new, _rx) = start_client(client_config(addr, "tok-a", "host-a", "new"));
    assert_eq!(
        tokio::time::timeout(WAIT, old.stopped()).await.unwrap(),
        StopReason::Fenced
    );
    release.send(()).unwrap();
    wait_until("first callback finished", || {
        first_report.lock().unwrap().is_some()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "queued notify ran on a fenced bridge"
    );
    let report = first_report.lock().unwrap().take().unwrap();
    assert!(
        matches!(
            report,
            Err(conde_remote::CallError::Stopped(StopReason::Fenced))
        ),
        "{report:?}"
    );
    assert!(
        source.record_calls_for("a") == 0,
        "outcome from a fenced bridge reached the server"
    );
    new.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: two outstanding calls that reuse one request_id each get their own response.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_calls_with_one_request_id_are_not_crosswired() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Gates {
        started: AtomicUsize,
        one: Mutex<std::sync::mpsc::Receiver<()>>,
        two: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Dispatcher for Gates {
        fn dispatch(&self, _: &Principal, r: Value) -> Value {
            let op = r["op"].as_str().unwrap().to_owned();
            let gate = if op == "one" { &self.one } else { &self.two };
            self.started.fetch_add(1, Ordering::SeqCst);
            gate.lock().unwrap().recv().unwrap();
            json!({"ok": true, "result": {"op": op}})
        }
    }
    let (tx1, rx1) = std::sync::mpsc::channel();
    let (tx2, rx2) = std::sync::mpsc::channel();
    let d = Arc::new(Gates {
        started: AtomicUsize::new(0),
        one: Mutex::new(rx1),
        two: Mutex::new(rx2),
    });
    let server = conde_remote::start(
        local(),
        test_config(),
        Arc::new(FakeAuth),
        d.clone(),
        Arc::new(FakeSource::default()),
    )
    .await
    .unwrap();
    let (client, _rx) = start_client(client_config(server.local_addr(), "tok-a", "host-a", "i-1"));

    let c = client.clone();
    let first = tokio::spawn(async move { c.call(json!({"request_id":"same","op":"one"})).await });
    wait_until("first dispatch", || d.started.load(Ordering::SeqCst) == 1).await;
    let c = client.clone();
    let second = tokio::spawn(async move { c.call(json!({"request_id":"same","op":"two"})).await });
    wait_until("second dispatch", || d.started.load(Ordering::SeqCst) == 2).await;

    tx1.send(()).unwrap();
    let r1 = tokio::time::timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tx2.send(()).unwrap();
    let r2 = tokio::time::timeout(WAIT, second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (&r1["result"]["op"], &r1["request_id"]),
        (&json!("one"), &json!("same"))
    );
    assert_eq!(
        (&r2["result"]["op"], &r2["request_id"]),
        (&json!("two"), &json!("same"))
    );
    client.shutdown();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoint_diffs_and_rejected_outcomes() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();
    let (client, _rx) = start_client(client_config(addr, "tok-a", "host-a", "i-1"));
    wait_until("hello endpoints", || {
        source.state().endpoint_updates.len() == 1
    })
    .await;
    let claude = |s: &str| Endpoint::Claude {
        session_id: s.into(),
    };
    let codex = Endpoint::Codex {
        thread_id: "th-1".into(),
    };

    // A session starts, then the codex thread goes away: one diff frame each.
    client.set_endpoints(vec![codex.clone(), claude("s-1")]);
    wait_until("added", || source.state().endpoint_updates.len() == 2).await;
    assert_eq!(source.state().endpoints, vec![claude("s-1"), codex]);
    client.set_endpoints(vec![claude("s-1")]);
    wait_until("removed", || source.state().endpoint_updates.len() == 3).await;
    assert_eq!(source.state().endpoints, vec![claude("s-1")]);
    // An unchanged set sends nothing.
    client.set_endpoints(vec![claude("s-1")]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(source.state().endpoint_updates.len(), 3);

    // A reconnect announces the current set in its hello.
    server.shutdown().await.unwrap();
    let server = start_server(addr, test_config(), source.clone()).await;
    wait_until("reconnect hello", || {
        source.state().endpoint_updates.len() == 4
    })
    .await;
    assert_eq!(source.state().endpoints, vec![claude("s-1")]);
    assert_eq!(client.connects(), 2);

    // A rejected outcome is acknowledged (no endless resend) and carries the connection.
    client.report("foreign-1", "denied", None).unwrap();
    wait_until("rejected acked", || client.unacked_outcomes() == 0).await;
    assert!(source.state().recorded.is_empty());
    assert_eq!(
        source.state().outcome_conns,
        vec![("host-a".to_owned(), "i-1".to_owned())]
    );
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: a pending snapshot taken before a takeover is never sent on the fenced
/// connection; that connection gets the fenced error instead.
#[tokio::test(flavor = "multi_thread")]
async fn snapshot_taken_before_a_takeover_is_not_sent_on_the_fenced_connection() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Source {
        base: FakeSource,
        ready: AtomicBool,
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl NotificationSource for Source {
        fn pending_for_host(&self, h: &str, i: &str) -> Vec<Notify> {
            let snapshot = {
                let s = self.base.state();
                if s.active.get(h).map(String::as_str) == Some(i) {
                    vec![notify("old-attempt")]
                } else {
                    vec![]
                }
            };
            // The old instance's first read stalls until the takeover is registered.
            if i == "old" && !self.ready.swap(true, Ordering::SeqCst) {
                self.gate.lock().unwrap().recv().unwrap();
            }
            snapshot
        }
        fn record_outcome(&self, h: &str, i: &str, o: &Outcome) -> Recorded {
            self.base.record_outcome(h, i, o)
        }
        fn fence(&self, h: &str, i: &str) -> FenceResult {
            self.base.fence(h, i)
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let source = Arc::new(Source {
        base: FakeSource::default(),
        ready: AtomicBool::new(false),
        gate: Mutex::new(rx),
    });
    let server = conde_remote::start(
        local(),
        test_config(),
        Arc::new(FakeAuth),
        Arc::new(FakeDispatcher),
        source.clone(),
    )
    .await
    .unwrap();
    let addr = server.local_addr();

    let mut old = raw_ws(addr, "tok-a").await.unwrap();
    send_json(&mut old, hello("host-a", "old")).await;
    wait_until("old snapshot captured", || {
        source.ready.load(Ordering::SeqCst)
    })
    .await;
    let mut new = raw_ws(addr, "tok-a").await.unwrap();
    send_json(&mut new, hello("host-a", "new")).await;
    // The new connection replays only after its registry entry is in place.
    assert_eq!(
        recv_json(&mut new).await.unwrap()["attempt_id"],
        "old-attempt"
    );
    tx.send(()).unwrap();

    let frame = recv_json(&mut old).await.expect("a frame before close");
    assert_eq!(
        (frame["type"].as_str(), frame["code"].as_str()),
        (Some("error"), Some("fenced")),
        "{frame}"
    );
    assert_eq!(recv_json(&mut old).await, None, "fenced connection closed");
    new.close(None).await.ok();
    server.shutdown().await.unwrap();
}

/// Review fixture: a source slower than the poll period must not starve requests or outcomes.
#[tokio::test(flavor = "multi_thread")]
async fn slow_empty_source_does_not_starve_requests_or_outcomes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Slow {
        polls: AtomicUsize,
        outcomes: AtomicUsize,
    }
    impl NotificationSource for Slow {
        fn pending_for_host(&self, _: &str, _: &str) -> Vec<Notify> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            vec![]
        }
        fn record_outcome(&self, _: &str, _: &str, _: &Outcome) -> Recorded {
            self.outcomes.fetch_add(1, Ordering::SeqCst);
            Recorded::Accepted
        }
        fn fence(&self, _: &str, _: &str) -> FenceResult {
            FenceResult::Active
        }
    }
    let source = Arc::new(Slow {
        polls: AtomicUsize::new(0),
        outcomes: AtomicUsize::new(0),
    });
    // poll_interval 50 ms < 80 ms per poll: a poll is due on every turn of the loop.
    let server = conde_remote::start(
        local(),
        test_config(),
        Arc::new(FakeAuth),
        Arc::new(FakeDispatcher),
        source.clone(),
    )
    .await
    .unwrap();
    let (client, _rx) = start_client(client_config(
        server.local_addr(),
        "tok-a",
        "host-a",
        "slow",
    ));
    wait_until("first poll", || source.polls.load(Ordering::SeqCst) > 0).await;

    let result = tokio::time::timeout(
        Duration::from_millis(600),
        client.call(json!({"op":"doctor"})),
    )
    .await;
    assert_eq!(result.expect("call starved").unwrap()["ok"], true);
    client.report("att-x", "delivered", None).unwrap();
    wait_until("outcome acked", || client.unacked_outcomes() == 0).await;
    assert_eq!(source.outcomes.load(Ordering::SeqCst), 1);
    assert!(
        source.polls.load(Ordering::SeqCst) >= 2,
        "polling continued"
    );
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// A transient store failure is answered with a retry ack; the client resends on its timer
/// over the same connection until the outcome lands, exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn transient_record_failures_are_resent_without_a_reconnect() {
    let source = Arc::new(FakeSource::default());
    source.add("host-a", "att-r");
    source.state().retry_next = 2;
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut cc = client_config(server.local_addr(), "tok-a", "host-a", "i-1");
    cc.outcome_resend_min = Duration::from_millis(100);
    cc.outcome_resend_max = Duration::from_millis(400);
    let (client, rx) = start_client(cc);
    assert_eq!(recv_notify(&rx).await.attempt_id, "att-r");
    let seq = client.report("att-r", "delivered", None).unwrap();

    wait_until("outcome landed", || client.unacked_outcomes() == 0).await;
    {
        let s = source.state();
        assert_eq!(
            s.record_calls,
            vec![("att-r".to_owned(), seq); 3],
            "two retries, then accepted"
        );
        assert_eq!(s.recorded.len(), 1);
        assert_eq!(
            s.fence_calls.len(),
            1,
            "resent on the live connection, no reconnect"
        );
    }
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Resubmissions keep attempt_id and event_seq, so an outcome whose write landed but was
/// answered `retry` is stored once.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_answered_after_the_write_is_deduplicated() {
    let source = Arc::new(FakeSource::default());
    source.add("host-a", "att-d");
    {
        let mut s = source.state();
        s.retry_next = 1;
        s.retry_after_record = true;
    }
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut cc = client_config(server.local_addr(), "tok-a", "host-a", "i-1");
    cc.outcome_resend_min = Duration::from_millis(100);
    let (client, rx) = start_client(cc);
    assert_eq!(recv_notify(&rx).await.attempt_id, "att-d");
    let seq = client.report("att-d", "delivered", None).unwrap();

    wait_until("outcome acked", || client.unacked_outcomes() == 0).await;
    {
        let s = source.state();
        assert_eq!(s.record_calls, vec![("att-d".to_owned(), seq); 2]);
        assert_eq!(s.recorded.len(), 1, "a resend was stored twice");
        assert_eq!(s.fence_calls.len(), 1);
    }
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: outcomes the store keeps answering `retry` slowly must not delay an RPC.
#[tokio::test(flavor = "multi_thread")]
async fn slow_retried_outcomes_do_not_starve_an_rpc() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct SlowRetry(AtomicUsize);
    impl NotificationSource for SlowRetry {
        fn pending_for_host(&self, _: &str, _: &str) -> Vec<Notify> {
            vec![]
        }
        fn fence(&self, _: &str, _: &str) -> FenceResult {
            FenceResult::Active
        }
        fn record_outcome(&self, _: &str, _: &str, _: &Outcome) -> Recorded {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            Recorded::Retry("store slow and unavailable".into())
        }
    }
    let source = Arc::new(SlowRetry(AtomicUsize::new(0)));
    let server = conde_remote::start(
        local(),
        test_config(),
        Arc::new(FakeAuth),
        Arc::new(FakeDispatcher),
        source.clone(),
    )
    .await
    .unwrap();
    let mut config = client_config(server.local_addr(), "tok-a", "host-a", "slow-retry");
    config.outcome_resend_min = Duration::from_millis(16);
    config.outcome_resend_max = Duration::from_millis(240);
    config.call_timeout = Duration::from_millis(300);
    let (client, _rx) = start_client(config);
    wait_connected(&client).await;
    for i in 0..4 {
        client
            .report(&format!("attempt-{i}"), "delivered", None)
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    let result = client.call(json!({"op":"probe"})).await;
    assert!(result.is_ok(), "{result:?}");
    // Serial 80 ms records over about 600 ms: the stream never outruns the store.
    let calls = source.0.load(Ordering::SeqCst);
    assert!(calls <= 9, "{calls} record calls");
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: one submission per outcome in flight, and at most four per connection.
#[tokio::test(flavor = "multi_thread")]
async fn outcome_submissions_are_one_per_outcome_and_capped_per_connection() {
    #[derive(Default)]
    struct Seen {
        outstanding: HashSet<(String, u64)>,
        max_outstanding: usize,
        resubmitted_while_outstanding: usize,
        submissions: HashMap<String, usize>,
    }
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let state = seen.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let (mut sink, mut stream) = ws.split();
        let (queue_tx, mut queue_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        // A serial store: 30 ms per outcome, `retry` for the first two submissions of each.
        let acker_state = state.clone();
        let acker = tokio::spawn(async move {
            while let Some(v) = queue_rx.recv().await {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let attempt = v["attempt_id"].as_str().unwrap().to_owned();
                let seq = v["event_seq"].as_u64().unwrap();
                let retry = {
                    let mut s = acker_state.lock().unwrap();
                    s.outstanding.remove(&(attempt.clone(), seq));
                    s.submissions[&attempt] <= 2
                };
                let ack = json!({"v":1,"type":"outcome_ack","attempt_id":attempt,"event_seq":seq,
                    "rejected":false,"retry":retry,"reason":null});
                if sink.send(Message::text(ack.to_string())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "outcome" {
                    let key = (
                        v["attempt_id"].as_str().unwrap().to_owned(),
                        v["event_seq"].as_u64().unwrap(),
                    );
                    {
                        let mut s = state.lock().unwrap();
                        if !s.outstanding.insert(key.clone()) {
                            s.resubmitted_while_outstanding += 1;
                        }
                        s.max_outstanding = s.max_outstanding.max(s.outstanding.len());
                        *s.submissions.entry(key.0).or_default() += 1;
                    }
                    queue_tx.send(v).unwrap();
                }
            }
        }
        acker.abort();
    });
    let mut config = client_config(addr, "tok-a", "host-a", "capped");
    config.outcome_resend_min = Duration::from_millis(10);
    config.outcome_resend_max = Duration::from_millis(40);
    let (client, _rx) = start_client(config);
    wait_connected(&client).await;
    for i in 0..8 {
        client
            .report(&format!("att-{i}"), "delivered", None)
            .unwrap();
    }
    wait_until("all outcomes accepted", || client.unacked_outcomes() == 0).await;
    {
        let s = seen.lock().unwrap();
        assert_eq!(
            s.resubmitted_while_outstanding, 0,
            "a submission overlapped its predecessor"
        );
        assert!(
            s.max_outstanding <= 4,
            "{} submissions outstanding",
            s.max_outstanding
        );
        assert_eq!(s.submissions.len(), 8);
        assert!(
            s.submissions.values().all(|n| *n == 3),
            "{:?}",
            s.submissions
        );
    }
    assert_eq!(client.connects(), 1);
    client.shutdown();
    let _ = tokio::time::timeout(WAIT, server).await;
}

/// Review fixture: retained outcomes are budgeted; a full budget refuses with Backpressure,
/// keeps nothing, and admits again once acks drain.
#[tokio::test(flavor = "multi_thread")]
async fn retained_outcomes_are_budgeted_with_backpressure() {
    use conde_remote::CallError;
    // Never accepted: no ack can drain the queue.
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let silent = listener.local_addr().unwrap();

    let (client, _rx) = start_client(client_config(silent, "tok-a", "host-a", "count-cap"));
    for i in 0..10_000 {
        client
            .report(&format!("att-{i}"), "delivered", None)
            .unwrap();
    }
    assert!(matches!(
        client.report("one-more", "delivered", None),
        Err(CallError::Backpressure)
    ));
    assert_eq!(client.unacked_outcomes(), 10_000);
    client.shutdown();

    let mut config = client_config(silent, "tok-a", "host-a", "byte-cap");
    config.max_unacked_bytes = 400;
    let (client, _rx) = start_client(config);
    // Over the whole budget: refused for good, and it never occupies the queue.
    let huge = "a".repeat(500);
    assert!(matches!(
        client.report(&huge, "delivered", None),
        Err(CallError::TooLarge { .. })
    ));
    assert_eq!(client.unacked_outcomes(), 0);
    let mut admitted = 0;
    while client
        .report(&format!("small-{admitted}"), "delivered", None)
        .is_ok()
    {
        admitted += 1;
    }
    assert!(admitted >= 2, "{admitted}");
    assert!(matches!(
        client.report("next", "delivered", None),
        Err(CallError::Backpressure)
    ));
    assert_eq!(client.unacked_outcomes(), admitted);
    client.shutdown();

    // Recoverable: once the server acknowledges, admission resumes.
    let source = Arc::new(FakeSource::default());
    source.state().retry_next = usize::MAX;
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut config = client_config(server.local_addr(), "tok-a", "host-a", "drain");
    config.max_unacked_outcomes = 2;
    config.outcome_resend_min = Duration::from_millis(20);
    config.outcome_resend_max = Duration::from_millis(40);
    let (client, _rx) = start_client(config);
    wait_connected(&client).await;
    client.report("d-1", "delivered", None).unwrap();
    client.report("d-2", "delivered", None).unwrap();
    assert!(matches!(
        client.report("d-3", "delivered", None),
        Err(CallError::Backpressure)
    ));
    source.state().retry_next = 0;
    wait_until("acks drained", || client.unacked_outcomes() == 0).await;
    client.report("d-3", "delivered", None).unwrap();
    client.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: an attempt's event_seq is never reused, however many attempts intervene,
/// so a later Delivered is never deduplicated as an earlier Held.
#[tokio::test(flavor = "current_thread")]
async fn event_seq_is_never_reused_for_an_attempt() {
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let mut config = client_config(listener.local_addr().unwrap(), "tok-a", "host-a", "seq");
    config.max_unacked_outcomes = 70_000;
    config.max_unacked_bytes = 64 * 1024 * 1024;
    let (client, _rx) = start_client(config);
    let held = client.report("old-pending", "held", None).unwrap();
    for i in 0..65_536 {
        client
            .report(&format!("new-{i}"), "delivered", None)
            .unwrap();
    }
    let delivered = client.report("old-pending", "delivered", None).unwrap();
    assert!(delivered > held, "held={held} delivered={delivered}");
    assert_eq!(client.unacked_outcomes(), 65_538);
    client.shutdown();
}

/// Review fixture: an ack that never arrives makes the client resubmit the identical outcome
/// on the same connection once the ack deadline passes.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_ack_resubmits_the_identical_outcome() {
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = frames.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "outcome" {
                    let count = {
                        let mut all = seen.lock().unwrap();
                        all.push(v.clone());
                        all.len()
                    };
                    if count == 1 {
                        continue; // the first ack is never sent
                    }
                    let ack = json!({"v":1,"type":"outcome_ack","attempt_id":v["attempt_id"],
                        "event_seq":v["event_seq"],"rejected":false,"retry":false,"reason":null});
                    ws.send(Message::text(ack.to_string())).await.unwrap();
                }
            }
        }
    });
    let mut config = client_config(addr, "tok-a", "host-a", "lost-ack");
    config.outcome_resend_min = Duration::from_millis(30);
    config.outcome_ack_timeout = Duration::from_millis(100);
    let (client, _rx) = start_client(config);
    wait_connected(&client).await;
    client.report("lost", "delivered", None).unwrap();
    wait_until("lost ack resubmitted", || client.unacked_outcomes() == 0).await;
    {
        let all = frames.lock().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], all[1]);
    }
    assert_eq!(client.connects(), 1);
    client.shutdown();
    tokio::time::timeout(WAIT, server).await.unwrap().unwrap();
}

/// Review fixture: a fenced client stops resubmitting its outcomes and refuses new ones.
#[tokio::test(flavor = "multi_thread")]
async fn fenced_client_stops_resubmitting_outcomes() {
    let source = Arc::new(FakeSource::default());
    source.state().retry_next = usize::MAX;
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut config = client_config(server.local_addr(), "tok-a", "host-a", "old-retry");
    config.outcome_resend_min = Duration::from_millis(20);
    config.outcome_resend_max = Duration::from_millis(40);
    let (old, _rx) = start_client(config);
    wait_connected(&old).await;
    old.report("old", "delivered", None).unwrap();
    wait_until("retries began", || source.record_calls_for("old") >= 2).await;
    let (new, _rx2) = start_client(client_config(
        server.local_addr(),
        "tok-a",
        "host-a",
        "new-fence",
    ));
    assert_eq!(
        tokio::time::timeout(WAIT, old.stopped()).await.unwrap(),
        StopReason::Fenced
    );
    let at_fence = source.record_calls_for("old");
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(source.record_calls_for("old"), at_fence);
    assert!(matches!(
        old.report("old", "delivered", None),
        Err(conde_remote::CallError::Stopped(StopReason::Fenced))
    ));
    new.shutdown();
    server.shutdown().await.unwrap();
}

/// Review fixture: an outcome that could never be sent must not wedge admission. A giant
/// reason is cut to MAX_REASON_BYTES and lands; a frame over the limit is refused for good.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_outcomes_never_block_admission() {
    use conde_remote::{CallError, MAX_REASON_BYTES};
    let source = Arc::new(FakeSource::default());
    let config = ServerConfig::default();
    let frame_limit = config.max_frame_bytes;
    let server = start_server(local(), config, source.clone()).await;
    let (client, _rx) = start_client(client_config(server.local_addr(), "tok-a", "host-a", "big"));
    wait_connected(&client).await;

    let reason = "x".repeat(frame_limit + 1);
    client
        .report("long-reason", "failed", Some(&reason))
        .unwrap();
    drop(reason);
    let huge_id = "a".repeat(frame_limit + 1);
    let refused = client.report(&huge_id, "failed", None);
    assert!(
        matches!(refused, Err(CallError::TooLarge { .. })),
        "{refused:?}"
    );
    drop(huge_id);
    client.report("next", "delivered", None).unwrap();

    wait_until("both outcomes acked", || client.unacked_outcomes() == 0).await;
    {
        let s = source.state();
        let long = s
            .recorded
            .iter()
            .find(|o| o.attempt_id == "long-reason")
            .unwrap();
        let stored = long.reason.as_deref().unwrap();
        assert!(
            stored.len() <= MAX_REASON_BYTES && stored.ends_with("[truncated]"),
            "{}",
            stored.len()
        );
        assert!(s.recorded.iter().any(|o| o.attempt_id == "next"));
        assert_eq!(s.fence_calls.len(), 1, "no reconnect loop");
    }
    client.shutdown();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoints_are_recorded_behind_the_outcomes_sent_before_them() {
    let source = Arc::new(FakeSource::default());
    source.state().slow_outcome = Duration::from_millis(300);
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut ws = raw_ws(server.local_addr(), "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-a", "i-1")).await;
    wait_until("hello snapshot", || !source.state().writes.is_empty()).await;

    // A hold, then the session's removal: the store sees them in that order even though the
    // hold takes a while to record (a removal written first would miss the hold).
    send_json(&mut ws, endpoints_frame(&["s1"], &[])).await;
    send_json(&mut ws, outcome_frame("slow-a")).await;
    send_json(&mut ws, endpoints_frame(&[], &["s1"])).await;
    send_json(&mut ws, outcome_frame("b")).await;
    wait_until("all written", || source.state().writes.len() == 5).await;
    assert_eq!(
        source.state().writes,
        vec!["endpoints []", "endpoints [s1]", "outcome slow-a", "endpoints []", "outcome b"]
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoints_write_that_fails_closes_the_connection_before_later_outcomes() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut ws = raw_ws(server.local_addr(), "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-a", "i-1")).await;
    wait_until("hello snapshot", || !source.state().writes.is_empty()).await;

    source.state().endpoints_fail_next = 1;
    send_json(&mut ws, endpoints_frame(&[], &["s1"])).await;
    send_json(&mut ws, outcome_frame("after")).await;
    assert_eq!(recv_close(&mut ws).await, Some((1011, "endpoints not recorded".into())));
    // The outcome read after the unrecorded snapshot is left for the bridge to resend.
    assert_eq!(source.state().writes, vec!["endpoints []"]);
    assert_eq!(source.record_calls_for("after"), 0);
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_connections_snapshot_does_not_overwrite_the_new_hello() {
    let source = Arc::new(FakeSource::default());
    let server = start_server(local(), test_config(), source.clone()).await;
    let addr = server.local_addr();
    let mut old = raw_ws(addr, "tok-a").await.unwrap();
    send_json(&mut old, hello("host-a", "i-1")).await;
    wait_until("hello snapshot", || !source.state().writes.is_empty()).await;

    // The old connection's snapshot is mid-write when the bridge reconnects with a fresh hello.
    source.state().hold_slow_endpoints = true;
    send_json(&mut old, endpoints_frame(&["slow"], &[])).await;
    wait_until("old snapshot writing", || source.state().slow_endpoints_writing).await;
    let mut new = raw_ws(addr, "tok-a").await.unwrap();
    let mut fresh = hello("host-a", "i-2");
    fresh["endpoints"] = json!([{"kind":"claude","session_id":"s2"}]);
    send_json(&mut new, fresh).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    source.state().hold_slow_endpoints = false;

    wait_until("new hello snapshot", || source.state().writes.contains(&"endpoints [s2]".to_string())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(source.state().endpoints, vec![claude("s2")]);
    assert_eq!(source.state().writes.last().map(String::as_str), Some("endpoints [s2]"));
    drop(old);
    drop(new);
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_work_queue_closes_the_connection_instead_of_pausing_it() {
    let source = Arc::new(FakeSource::default());
    source.state().slow_outcome = Duration::from_secs(2);
    let server = start_server(local(), test_config(), source.clone()).await;
    let mut ws = raw_ws(server.local_addr(), "tok-a").await.unwrap();
    send_json(&mut ws, hello("host-a", "i-1")).await;
    wait_until("hello snapshot", || !source.state().writes.is_empty()).await;

    // One outcome records slowly; the 64 behind it fill the queue and the next one is refused.
    for i in 0..70 {
        send_json(&mut ws, outcome_frame(&format!("slow-{i}"))).await;
    }
    let t = tokio::time::Instant::now();
    assert_eq!(recv_close(&mut ws).await, Some((1011, "work queue full".into())));
    assert!(t.elapsed() < Duration::from_secs(1), "closed after {:?}", t.elapsed());
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_closed_right_after_connecting_keep_the_backoff_growing() {
    // A server that accepts each connection and closes it at once (as for a full queue).
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    let _ = ws.next().await;
                    let _ = ws.close(None).await;
                }
            });
        }
    });
    let mut config = client_config(addr, "tok-a", "host-a", "i-1");
    config.backoff_min = Duration::from_millis(40);
    config.backoff_max = Duration::from_secs(2);
    let (client, _rx) = start_client(config);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Doubling from 40 ms (jittered to 20..40, 40..80, ...) allows about 6 sessions in 1.5 s;
    // a backoff reset on every connect would allow some 50.
    let connects = client.connects();
    client.shutdown();
    assert!((2..=10).contains(&connects), "{connects} connects in 1.5 s");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_change_goes_out_before_an_outcome_reported_after_it() {
    // A server that reads the hello, then reads nothing until `resume`, then reports the
    // endpoints and outcome frames in the order they arrived.
    let listener = tokio::net::TcpListener::bind(local()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let resume = Arc::new(tokio::sync::Notify::new());
    let (stuck_tx, stuck_rx) = tokio::sync::oneshot::channel::<()>();
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let gate = resume.clone();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let unbounded = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let mut ws = tokio_tungstenite::accept_async_with_config(tcp, Some(unbounded)).await.unwrap();
        let _hello = ws.next().await;
        // The big request has started to arrive: its sender is stuck until this side reads.
        let mut buf = vec![0u8; 1 << 16];
        while ws.get_ref().peek(&mut buf).await.unwrap() < buf.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = stuck_tx.send(());
        gate.notified().await;
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "endpoints" || v["type"] == "outcome" {
                    let _ = seen_tx.send(v["type"].as_str().unwrap().to_string());
                }
            }
        }
    });
    let (client, _rx) = start_client(client_config(addr, "tok-a", "host-a", "i-1"));
    wait_connected(&client).await;

    // The writer is stuck on a request larger than the socket buffers while the daemon adds a
    // session and reports a hold for it.
    let big = client.clone();
    tokio::spawn(async move {
        let _ = big.call(json!({"op": "big", "pad": "x".repeat(32 << 20)})).await;
    });
    tokio::time::timeout(WAIT, stuck_rx).await.expect("big request on the wire").unwrap();
    client.set_endpoints(vec![
        Endpoint::Codex { thread_id: "th-1".into() },
        Endpoint::Codex { thread_id: "th-2".into() },
    ]);
    client.report("att-th-2", "held", None).unwrap();
    resume.notify_one();

    let mut order = Vec::new();
    while order.len() < 2 {
        let kind = tokio::time::timeout(WAIT, seen_rx.recv()).await.expect("frames arrive").unwrap();
        order.push(kind);
    }
    assert_eq!(order, ["endpoints", "outcome"]);
    client.shutdown();
}

/// A board that moved (`moved_to`): `/healthz` still answers, every other request is a 308 to
/// the new address with `{"moved_to"}`; a bridge stops for good on it (no reconnect) and an RPC
/// call reports it, both as [`StopReason::Moved`].
#[tokio::test(flavor = "multi_thread")]
async fn a_moved_board_answers_308_everywhere_but_healthz_and_clients_stop() {
    const NEW: &str = "https://dakai.conde.cloud";
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = ServerConfig { moved_to: Some(NEW.into()), ..test_config() };
    let server = start_server(local(), config, Arc::default()).await;
    let addr = server.local_addr();
    assert_eq!(http(addr, "GET", "/healthz", None, "").await, (200, "ok".into()));
    let web = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    for (method, path) in [("POST", "/v1/rpc"), ("GET", "/v1/bridge"), ("GET", "/keys"), ("POST", "/v1/admin/users"), ("GET", "/install.sh"), ("GET", "/no/such/path")] {
        let r = web
            .request(method.parse().unwrap(), format!("http://{addr}{path}"))
            .bearer_auth("tok-a")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 308, "{method} {path}");
        assert_eq!(r.headers()["location"], NEW, "{method} {path}");
        assert_eq!(r.json::<Value>().await.unwrap(), json!({"moved_to": NEW}), "{method} {path}");
    }
    match raw_ws(addr, "tok-a").await {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 308),
        other => panic!("expected a 308 handshake, got {:?}", other.map(|_| ())),
    }
    let called = conde_remote::rpc(&format!("http://{addr}/v1/rpc"), "tok-a", json!({"op": "board.info", "body": {}}), WAIT).await;
    assert!(matches!(&called, Err(CallError::Stopped(StopReason::Moved(url))) if url == NEW), "{called:?}");

    let (client, _rx) = start_client(client_config(addr, "tok-a", "host-a", "i-moved"));
    let reason = tokio::time::timeout(WAIT, client.stopped()).await.unwrap();
    assert_eq!(reason, StopReason::Moved(NEW.into()));
    assert_eq!(client.connects(), 0, "never connected, never retried");
    server.shutdown().await.unwrap();
}

#[test]
fn a_move_is_read_from_the_body_else_the_location() {
    assert_eq!(conde_remote::moved_to(308, Some("https://b"), br#"{"moved_to":"https://a"}"#).as_deref(), Some("https://a"));
    assert_eq!(conde_remote::moved_to(308, Some("https://b"), b"not json").as_deref(), Some("https://b"));
    assert_eq!(conde_remote::moved_to(308, None, b""), None);
    assert_eq!(conde_remote::moved_to(307, Some("https://b"), br#"{"moved_to":"https://a"}"#), None);
}
