#![allow(dead_code)]
//! A scripted v2 shared board for the daemon's gate tests (DESIGN-v2.md §4-6 as the daemon
//! sees them): `/v1/rpc` and `/v1/bridge` with the v2 hello, fencing, the admission window,
//! structured notifies, `routes` frames, outcome acks with event_seq de-duplication, the
//! stale-attempt rule, `mailbox.*` with generations, and a small DM board for mailbox actors.
//! The test controls acks, connections, fences and credentials, and every inbound frame and
//! RPC body is captured. It stands in for the V1 server until that lane lands.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use conde_remote::{Endpoint, Hello, Notify, NotifyFields, NotifyKind, OutcomeReason, OutcomeStatus, Route, Routes};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// What the board does with an outcome frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acks {
    /// Record it and acknowledge it.
    Normal,
    /// Neither record nor acknowledge (a stalled board): first outcomes keep the window full.
    Stall,
    /// Record it, but the ack is lost.
    LoseAck,
}

#[derive(Clone, Debug)]
pub struct Mailbox {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub host: Option<String>,
    pub generation: u64,
    pub human: bool,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: String,
    pub from: String,
    pub to: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub read: bool,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub msg: String,
    pub mailbox: String,
    pub status: String,
    pub next_at: Instant,
    pub current: Option<String>,
    /// A hand-made notify ([`FakeBoard::push_raw`]): never claimed or re-attempted.
    pub raw: bool,
}

#[derive(Clone, Debug)]
pub struct Attempt {
    pub id: String,
    pub msg: String,
    pub instance: String,
    pub host: String,
    pub generation: u64,
    /// None while dispatching (no outcome recorded).
    pub status: Option<String>,
    pub reason: Option<String>,
    pub last_seq: u64,
    /// Every status recorded, in order.
    pub history: Vec<String>,
}

struct HostRow {
    owner: String,
    revoked: bool,
    mailbox_only: bool,
    active: Option<String>,
    fenced: HashSet<String>,
    acks: Acks,
    endpoints: HashSet<String>,
}

enum Cmd {
    Frame(String),
    Close,
}

#[derive(Default)]
struct St {
    hosts: HashMap<String, HostRow>,
    keys: HashMap<String, String>,
    mailboxes: Vec<Mailbox>,
    messages: Vec<Msg>,
    notes: HashMap<String, Note>,
    attempts: HashMap<String, Attempt>,
    idem: HashMap<(String, String, String), Value>,
    /// (host, frame or RPC body) of everything the hosts sent.
    capture: Vec<(String, String)>,
    /// (host, frame) of every notify pushed.
    pushed: Vec<(String, Value)>,
    conns: HashMap<String, (String, mpsc::UnboundedSender<Cmd>, u64)>,
    conn_seq: u64,
    /// Connections established per host, ever.
    connects: HashMap<String, u64>,
    retry_ms: u64,
    /// The next answer of this op is lost: the connection drops after the commit.
    lose_response: HashSet<String>,
    /// Ops committed without an answer.
    silent: HashSet<String>,
    windows: HashMap<String, u32>,
    seq: u64,
    /// No `routes` frame on connect (the daemon then reconciles with `mailbox.mine`).
    no_routes_on_connect: bool,
}

impl St {
    fn ulid(&mut self) -> String {
        self.seq += 1;
        format!("{}{:06}", conde::util::new_ulid(), self.seq)[..26].to_string()
    }

    fn mailbox(&self, id: &str) -> Option<&Mailbox> {
        self.mailboxes.iter().find(|m| m.id == id)
    }

    fn canonical(&self, id: &str) -> String {
        self.mailbox(id).map(|m| format!("{}/{}", m.owner, m.name)).unwrap_or_default()
    }

    fn send_to(&self, host: &str, frame: String) {
        if let Some((_, tx, _)) = self.conns.get(host) {
            let _ = tx.send(Cmd::Frame(frame));
        }
    }

    fn routes_frame(&self, host: &str) -> String {
        let mailboxes: Vec<Route> = self
            .mailboxes
            .iter()
            .filter(|m| m.host.as_deref() == Some(host))
            .map(|m| Route { mailbox_id: m.id.clone(), route_generation: m.generation })
            .collect();
        frame("routes", &serde_json::to_value(Routes { mailboxes }).unwrap())
    }

    /// The stale-attempt rule: an open attempt whose instance is not the host's active one, or
    /// whose generation is not its mailbox's, is abandoned and re-attempted at once.
    fn abandon_stale(&mut self) {
        let mut stale = Vec::new();
        for a in self.attempts.values() {
            let open = matches!(a.status.as_deref(), None | Some("held") | Some("accepted"));
            if !open {
                continue;
            }
            let note = &self.notes[&a.msg];
            if note.raw {
                continue;
            }
            if note.current.as_deref() != Some(&a.id) || matches!(note.status.as_str(), "delivered" | "refused" | "denied") {
                continue;
            }
            let mb = self.mailbox(&note.mailbox).cloned();
            let host_active = self.hosts.get(&a.host).and_then(|h| h.active.clone());
            let moved = mb.as_ref().is_none_or(|m| m.generation != a.generation || m.host.as_deref() != Some(&a.host));
            if moved || host_active.as_deref() != Some(&a.instance) {
                stale.push(a.id.clone());
            }
        }
        for id in stale {
            self.settle(&id, "abandoned", Some("stale".into()));
        }
    }

    fn settle(&mut self, attempt: &str, status: &str, reason: Option<String>) {
        let retry = Duration::from_millis(self.retry_ms);
        let Some(a) = self.attempts.get_mut(attempt) else { return };
        a.status = Some(status.to_string());
        a.reason = reason;
        a.history.push(status.to_string());
        let msg = a.msg.clone();
        let note = self.notes.get_mut(&msg).unwrap();
        if note.current.as_deref() != Some(attempt) {
            return;
        }
        let now = Instant::now();
        match status {
            "accepted" => note.status = "notified".into(),
            "held" => note.status = "held".into(),
            "delivered" => note.status = "delivered".into(),
            "denied" | "refused" => note.status = status.into(),
            "abandoned" => {
                note.status = "pending".into();
                note.next_at = now;
            }
            _ => {
                note.status = "pending".into();
                note.next_at = now + retry;
            }
        }
    }

    /// Claims due notifications for `instance` of `host` up to its window.
    fn claim(&mut self, host: &str, instance: &str) -> Vec<Value> {
        self.abandon_stale();
        let window = *self.windows.get(instance).unwrap_or(&32) as usize;
        let dispatching = self.attempts.values().filter(|a| a.instance == instance && a.status.is_none()).count();
        let mut room = window.saturating_sub(dispatching);
        let now = Instant::now();
        let mut due: Vec<String> = self
            .notes
            .iter()
            .filter(|(_, n)| !n.raw && n.status == "pending" && n.next_at <= now)
            .filter(|(_, n)| self.mailbox(&n.mailbox).is_some_and(|m| m.host.as_deref() == Some(host)))
            .map(|(k, _)| k.clone())
            .collect();
        due.sort();
        let mut out = Vec::new();
        for msg in due {
            if room == 0 {
                break;
            }
            let m = self.messages.iter().find(|x| x.id == msg).cloned().unwrap();
            if m.read {
                self.notes.get_mut(&msg).unwrap().status = "read".into();
                continue;
            }
            let mb = self.mailbox(&self.notes[&msg].mailbox).cloned().unwrap();
            if mb.human {
                self.notes.get_mut(&msg).unwrap().status = "badge".into();
                continue;
            }
            let id = self.ulid();
            self.attempts.insert(
                id.clone(),
                Attempt {
                    id: id.clone(),
                    msg: msg.clone(),
                    instance: instance.into(),
                    host: host.into(),
                    generation: mb.generation,
                    status: None,
                    reason: None,
                    last_seq: 0,
                    history: Vec::new(),
                },
            );
            let note = self.notes.get_mut(&msg).unwrap();
            note.status = "dispatching".into();
            note.current = Some(id.clone());
            room -= 1;
            out.push(self.notify_frame(&id));
        }
        out
    }

    fn notify_frame(&self, attempt: &str) -> Value {
        let a = &self.attempts[attempt];
        let note = &self.notes[&a.msg];
        let m = self.messages.iter().find(|x| x.id == a.msg).unwrap();
        let n = Notify {
            notification_id: format!("{}.{}", a.msg, note.mailbox),
            attempt_id: a.id.clone(),
            recipient: self.canonical(&note.mailbox),
            binding_version: a.generation,
            endpoint: Endpoint::Mailbox { mailbox_id: note.mailbox.clone() },
            message_id: m.id.clone(),
            text: String::new(),
            fields: Some(NotifyFields {
                kind: NotifyKind::Dm,
                sender: self.canonical(&m.from),
                place: "dm".into(),
                badges: Vec::new(),
                member: true,
            }),
        };
        let mut v = serde_json::to_value(n).unwrap();
        v["v"] = json!(1);
        v["type"] = json!("notify");
        v
    }

    fn open_attempts(&self, instance: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .attempts
            .values()
            .filter(|a| a.instance == instance && matches!(a.status.as_deref(), None | Some("held")))
            .filter(|a| self.notes[&a.msg].current.as_deref() == Some(&a.id))
            .map(|a| a.id.clone())
            .collect();
        ids.sort();
        ids
    }
}

fn frame(kind: &str, payload: &Value) -> String {
    let mut v = payload.clone();
    v["v"] = json!(1);
    v["type"] = json!(kind);
    v.to_string()
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"v": 1, "type": "response", "request_id": id, "ok": true, "result": result})
}

fn err(id: &Value, code: &str, message: &str) -> Value {
    json!({"v": 1, "type": "response", "request_id": id, "ok": false, "error": {"code": code, "message": message}})
}

#[derive(Clone)]
pub struct FakeBoard {
    st: Arc<Mutex<St>>,
    pub addr: SocketAddr,
    rt: Arc<tokio::runtime::Runtime>,
}

impl FakeBoard {
    pub fn start() -> FakeBoard {
        let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap());
        let st = Arc::new(Mutex::new(St { retry_ms: 300, ..St::default() }));
        let app = Router::new()
            .route("/v1/rpc", post(rpc))
            .route("/v1/bridge", any(bridge))
            .with_state(st.clone());
        let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();
        rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
        let board = FakeBoard { st, addr, rt };
        let pump = board.clone();
        board.rt.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                pump.pump();
            }
        });
        board
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Pushes due notifications to every connected host, up to its window.
    fn pump(&self) {
        let mut st = self.lock();
        let conns: Vec<(String, String)> = st.conns.iter().map(|(h, (i, _, _))| (h.clone(), i.clone())).collect();
        for (host, instance) in conns {
            for n in st.claim(&host, &instance) {
                st.pushed.push((host.clone(), n.clone()));
                st.send_to(&host, n.to_string());
            }
        }
    }

    /// A key for `owner` on `host` (the host is not migrated until `host.migrate_v2`).
    pub fn enroll(&self, owner: &str, host: &str) -> String {
        let key = format!("key-{host}-{}", conde::util::new_ulid());
        let mut st = self.lock();
        st.keys.insert(key.clone(), host.into());
        st.hosts.insert(
            host.into(),
            HostRow {
                owner: owner.into(),
                revoked: false,
                mailbox_only: false,
                active: None,
                fenced: HashSet::new(),
                acks: Acks::Normal,
                endpoints: HashSet::new(),
            },
        );
        key
    }

    /// A teammate's mailbox, bound to `host`.
    pub fn mailbox(&self, owner: &str, name: &str, host: &str) -> String {
        let mut st = self.lock();
        let id = st.ulid();
        st.mailboxes.push(Mailbox { id: id.clone(), owner: owner.into(), name: name.into(), host: Some(host.into()), generation: 1, human: false });
        id
    }

    pub fn mailboxes(&self) -> Vec<Mailbox> {
        self.lock().mailboxes.clone()
    }

    pub fn mailbox_named(&self, owner: &str, name: &str) -> Option<Mailbox> {
        self.lock().mailboxes.iter().find(|m| m.owner == owner && m.name == name).cloned()
    }

    /// A DM from mailbox `from` to `owner/name` written on the board directly (a teammate's
    /// agent sending through its own daemon is the same row).
    pub fn dm(&self, from: &str, to_owner: &str, to_name: &str, text: &str) -> String {
        let mut st = self.lock();
        let to = st.mailboxes.iter().find(|m| m.owner == to_owner && m.name == to_name).unwrap().id.clone();
        let id = st.ulid();
        st.messages.push(Msg { id: id.clone(), from: from.into(), to: to.clone(), text: text.into(), reply_to: None, read: false });
        st.notes.insert(id.clone(), Note { msg: id.clone(), mailbox: to, status: "pending".into(), next_at: Instant::now(), current: None, raw: false });
        id
    }

    /// Pushes a hand-made notify (its endpoint, generation and fields as given) to the host's
    /// connection; returns its attempt id. Its outcomes are captured, never re-attempted.
    pub fn push_raw(&self, host: &str, mut n: Notify) -> String {
        let mut st = self.lock();
        let attempt = st.ulid();
        let msg = st.ulid();
        n.attempt_id = attempt.clone();
        n.message_id = msg.clone();
        let instance = st.conns.get(host).map(|c| c.0.clone()).expect("connected host");
        let mailbox = match &n.endpoint {
            Endpoint::Mailbox { mailbox_id } => mailbox_id.clone(),
            _ => String::new(),
        };
        st.messages.push(Msg { id: msg.clone(), from: String::new(), to: mailbox.clone(), text: "raw".into(), reply_to: None, read: false });
        st.notes.insert(msg.clone(), Note { msg: msg.clone(), mailbox, status: "dispatching".into(), next_at: Instant::now(), current: Some(attempt.clone()), raw: true });
        st.attempts.insert(
            attempt.clone(),
            Attempt { id: attempt.clone(), msg, instance, host: host.into(), generation: n.binding_version, status: None, reason: None, last_seq: 0, history: Vec::new() },
        );
        let mut v = serde_json::to_value(&n).unwrap();
        v["v"] = json!(1);
        v["type"] = json!("notify");
        st.pushed.push((host.into(), v.clone()));
        st.send_to(host, v.to_string());
        attempt
    }

    /// Another host of the owner takes `owner/name` over (what `publish --take` from that host
    /// does on the board): a new generation, the old host's attempts re-attempted, its routes sent.
    pub fn take(&self, owner: &str, name: &str, host: &str) -> u64 {
        let mut st = self.lock();
        let m = st.mailboxes.iter_mut().find(|m| m.owner == owner && m.name == name).unwrap();
        let old = m.host.replace(host.into());
        m.generation += 1;
        let generation = m.generation;
        st.abandon_stale();
        if let Some(old) = old {
            let f = st.routes_frame(&old);
            st.send_to(&old, f);
        }
        generation
    }

    pub fn routes_on_connect(&self, on: bool) {
        self.lock().no_routes_on_connect = !on;
    }

    /// The board commits `op` but sends no answer until [`FakeBoard::answer`].
    pub fn silence(&self, op: &str) {
        self.lock().silent.insert(op.into());
    }

    pub fn answer(&self, op: &str) {
        self.lock().silent.remove(op);
    }

    /// Sends the host a `routes` frame with its current routes.
    pub fn send_routes(&self, host: &str) {
        let st = self.lock();
        let f = st.routes_frame(host);
        st.send_to(host, f);
    }

    /// Outcomes the host reported for one attempt: (event_seq, status, reason).
    pub fn outcomes_of(&self, host: &str, attempt: &str) -> Vec<(u64, String, Option<String>)> {
        self.outcome_frames(host).into_iter().filter(|o| o.0 == attempt).map(|o| (o.1, o.2, o.3)).collect()
    }

    pub fn set_acks(&self, host: &str, acks: Acks) {
        self.lock().hosts.get_mut(host).unwrap().acks = acks;
    }

    /// Drops the host's connection (the process on the host stays).
    pub fn drop_connection(&self, host: &str) {
        if let Some((_, tx, _)) = self.lock().conns.remove(host) {
            let _ = tx.send(Cmd::Close);
        }
    }

    pub fn revoke(&self, host: &str) {
        let mut st = self.lock();
        st.hosts.get_mut(host).unwrap().revoked = true;
        if let Some((_, tx, _)) = st.conns.remove(host) {
            let _ = tx.send(Cmd::Close);
        }
    }

    /// Another instance of the host takes over: the connected one is fenced.
    pub fn fence(&self, host: &str) {
        let mut st = self.lock();
        let newer = format!("other-{}", conde::util::new_ulid());
        let row = st.hosts.get_mut(host).unwrap();
        if let Some(old) = row.active.replace(newer) {
            row.fenced.insert(old);
        }
        if let Some((_, tx, _)) = st.conns.remove(host) {
            let _ = tx.send(Cmd::Frame(frame("error", &json!({"code": "fenced", "message": "a newer bridge took over"}))));
            let _ = tx.send(Cmd::Close);
        }
    }

    /// The next answer of `op` is lost after its commit: the connection drops instead.
    pub fn lose_response(&self, op: &str) {
        self.lock().lose_response.insert(op.into());
    }

    /// Frames and RPC bodies the host sent, in order.
    pub fn capture(&self, host: &str) -> Vec<String> {
        self.lock().capture.iter().filter(|(h, _)| h == host).map(|(_, f)| f.clone()).collect()
    }

    pub fn capture_all(&self) -> String {
        self.lock().capture.iter().map(|(h, f)| format!("{h} {f}\n")).collect()
    }

    /// Outcome frames the host sent for messages' attempts: (attempt, event_seq, status, reason).
    pub fn outcome_frames(&self, host: &str) -> Vec<(String, u64, String, Option<String>)> {
        self.capture(host)
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(f).ok())
            .filter(|v| v["type"] == "outcome")
            .map(|v| {
                (
                    v["attempt_id"].as_str().unwrap_or_default().to_string(),
                    v["event_seq"].as_u64().unwrap_or_default(),
                    v["status"].as_str().unwrap_or_default().to_string(),
                    v["reason"].as_str().map(str::to_string),
                )
            })
            .collect()
    }

    /// Every attempt of a message, oldest first.
    pub fn attempts(&self, msg: &str) -> Vec<Attempt> {
        let mut a: Vec<Attempt> = self.lock().attempts.values().filter(|a| a.msg == msg).cloned().collect();
        a.sort_by(|x, y| x.id.cmp(&y.id));
        a
    }

    pub fn note(&self, msg: &str) -> Note {
        self.lock().notes[msg].clone()
    }

    /// Notifies pushed for a message: (host, attempt).
    pub fn pushes(&self, msg: &str) -> Vec<(String, String)> {
        self.lock()
            .pushed
            .iter()
            .filter(|(_, f)| f["message_id"] == msg)
            .map(|(h, f)| (h.clone(), f["attempt_id"].as_str().unwrap_or_default().to_string()))
            .collect()
    }

    pub fn dispatching(&self, host: &str) -> usize {
        self.lock().attempts.values().filter(|a| a.host == host && a.status.is_none()).count()
    }

    pub fn connected(&self, host: &str) -> bool {
        self.lock().conns.contains_key(host)
    }

    /// Connections the host established so far.
    pub fn connections(&self, host: &str) -> u64 {
        self.lock().connects.get(host).copied().unwrap_or_default()
    }

    pub fn endpoints(&self, host: &str) -> HashSet<String> {
        self.lock().hosts[host].endpoints.clone()
    }

    pub fn messages(&self) -> Vec<Msg> {
        self.lock().messages.clone()
    }

    pub fn set_retry(&self, ms: u64) {
        self.lock().retry_ms = ms;
    }

    /// Fleet "reached" for a mailbox row: a delivered attempt (mailbox rows count delivered only).
    pub fn reached(&self, msg: &str) -> bool {
        self.attempts(msg).iter().any(|a| a.history.iter().any(|s| s == "delivered"))
    }
}

fn host_of(st: &St, headers: &HeaderMap) -> Option<String> {
    let token = headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")?;
    let host = st.keys.get(token)?;
    (!st.hosts.get(host)?.revoked).then(|| host.clone())
}

async fn rpc(State(st): State<Arc<Mutex<St>>>, headers: HeaderMap, body: Bytes) -> Response {
    let mut s = st.lock().unwrap();
    let Some(host) = host_of(&s, &headers) else { return StatusCode::UNAUTHORIZED.into_response() };
    let text = String::from_utf8_lossy(&body).to_string();
    s.capture.push((host.clone(), format!("POST /v1/rpc {text}")));
    let req: Value = serde_json::from_str(&text).unwrap_or_default();
    let resp = dispatch(&mut s, &host, &req);
    axum::Json(resp).into_response()
}

async fn bridge(State(st): State<Arc<Mutex<St>>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let host = {
        let s = st.lock().unwrap();
        host_of(&s, &headers)
    };
    let Some(host) = host else { return StatusCode::UNAUTHORIZED.into_response() };
    ws.on_upgrade(move |socket| session(st, host, socket))
}

async fn session(st: Arc<Mutex<St>>, host: String, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let Some(Ok(Message::Text(text))) = rx.next().await else { return };
    let hello_text = text.to_string();
    let (instance, refused) = {
        let mut s = st.lock().unwrap();
        s.capture.push((host.clone(), hello_text.clone()));
        let v: Value = serde_json::from_str(&hello_text).unwrap_or_default();
        match serde_json::from_value::<Hello>(v) {
            Ok(h) if h.check().is_ok() && h.proto == 2 && h.host_id == host => {
                let row = s.hosts.get_mut(&host).unwrap();
                if row.fenced.contains(&h.bridge_instance) {
                    (h.bridge_instance, Some(("fenced", "a newer bridge took over")))
                } else {
                    if let Some(old) = row.active.replace(h.bridge_instance.clone())
                        && old != h.bridge_instance
                    {
                        row.fenced.insert(old);
                    }
                    row.endpoints = h
                        .endpoints
                        .iter()
                        .filter_map(|e| match e {
                            Endpoint::Mailbox { mailbox_id } => Some(mailbox_id.clone()),
                            _ => None,
                        })
                        .collect();
                    s.windows.insert(h.bridge_instance.clone(), h.window.unwrap_or(32));
                    (h.bridge_instance, None)
                }
            }
            _ => (String::new(), Some(("bad_hello", "upgrade conde"))),
        }
    };
    if let Some((code, message)) = refused {
        let _ = tx.send(Message::text(frame("error", &json!({"code": code, "message": message})))).await;
        return;
    }
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let my_conn;
    {
        let mut s = st.lock().unwrap();
        s.conn_seq += 1;
        my_conn = s.conn_seq;
        let connects = {
            let n = s.connects.entry(host.clone()).or_default();
            *n += 1;
            *n
        };
        if let Some((_, old, _)) = s.conns.insert(host.clone(), (instance.clone(), cmd_tx.clone(), connects)) {
            let _ = old.send(Cmd::Frame(frame("error", &json!({"code": "fenced", "message": "a newer bridge took over"}))));
            let _ = old.send(Cmd::Close);
        }
        if !s.no_routes_on_connect {
            let routes = s.routes_frame(&host);
            let _ = cmd_tx.send(Cmd::Frame(routes));
        }
        // Open attempts of this instance are offered again on a new connection.
        for id in s.open_attempts(&instance) {
            let n = s.notify_frame(&id);
            s.pushed.push((host.clone(), n.clone()));
            let _ = cmd_tx.send(Cmd::Frame(n.to_string()));
        }
    }
    let _ = my_conn;
    let ping = tokio::time::interval(Duration::from_millis(500));
    tokio::pin!(ping);
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(Cmd::Frame(f)) => if tx.send(Message::text(f)).await.is_err() { break },
                Some(Cmd::Close) | None => {
                    let _ = tx.send(Message::Close(None)).await;
                    break;
                }
            },
            _ = ping.tick() => if tx.send(Message::Ping(Bytes::new())).await.is_err() { break },
            msg = rx.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let replies = {
                    let mut s = st.lock().unwrap();
                    s.capture.push((host.clone(), text.clone()));
                    handle_frame(&mut s, &host, &instance, &text)
                };
                for r in replies {
                    match r {
                        Reply::Frame(f) => if tx.send(Message::text(f)).await.is_err() { break },
                        Reply::Drop => {
                            let mut s = st.lock().unwrap();
                            if s.conns.get(&host).is_some_and(|c| c.0 == instance) {
                                s.conns.remove(&host);
                            }
                            return;
                        }
                    }
                }
            }
        }
    }
    let mut s = st.lock().unwrap();
    if s.conns.get(&host).is_some_and(|c| c.0 == instance && c.1.same_channel(&cmd_tx)) {
        s.conns.remove(&host);
    }
}

enum Reply {
    Frame(String),
    Drop,
}

fn handle_frame(s: &mut St, host: &str, instance: &str, text: &str) -> Vec<Reply> {
    let v: Value = serde_json::from_str(text).unwrap_or_default();
    match v["type"].as_str().unwrap_or_default() {
        "outcome" => {
            let attempt = v["attempt_id"].as_str().unwrap_or_default().to_string();
            let seq = v["event_seq"].as_u64().unwrap_or_default();
            let status = v["status"].as_str().unwrap_or_default().to_string();
            let reason = v["reason"].as_str().map(str::to_string);
            // A v2 outcome carries a known status and a reason from the closed set.
            if OutcomeStatus::parse(&status).is_none() || reason.as_deref().is_some_and(|r| OutcomeReason::parse(r).is_none()) {
                return vec![Reply::Frame(frame("outcome_ack", &json!({"attempt_id": attempt, "event_seq": seq, "rejected": true, "reason": "outside the closed set"})))];
            }
            let acks = s.hosts[host].acks;
            if acks == Acks::Stall {
                return Vec::new();
            }
            let mut ack = json!({"attempt_id": attempt, "event_seq": seq});
            match s.attempts.get(&attempt).cloned() {
                None => ack["rejected"] = json!(true),
                Some(a) if a.instance != instance => ack["rejected"] = json!(true),
                Some(a) if seq <= a.last_seq => ack["duplicate"] = json!(true),
                Some(a) => {
                    s.attempts.get_mut(&attempt).unwrap().last_seq = seq;
                    let terminal = a.status.as_deref().is_some_and(|x| !matches!(x, "accepted" | "held"));
                    let moved = s.mailbox(&s.notes[&a.msg].mailbox).is_none_or(|m| m.generation != a.generation);
                    if terminal {
                        // already settled; ignored
                    } else if moved {
                        // An outcome for a stale generation never ends the notification.
                        s.settle(&attempt, "abandoned", Some("route moved".into()));
                    } else {
                        s.settle(&attempt, &status, reason);
                    }
                }
            }
            if acks == Acks::LoseAck {
                return Vec::new();
            }
            vec![Reply::Frame(frame("outcome_ack", &ack))]
        }
        "endpoints" => {
            let row = s.hosts.get_mut(host).unwrap();
            for e in v["added"].as_array().into_iter().flatten() {
                if let Some(id) = e["mailbox_id"].as_str() {
                    row.endpoints.insert(id.into());
                }
            }
            for e in v["removed"].as_array().into_iter().flatten() {
                if let Some(id) = e["mailbox_id"].as_str() {
                    row.endpoints.remove(id);
                }
            }
            Vec::new()
        }
        "request" => {
            let op = v["op"].as_str().unwrap_or_default().to_string();
            let resp = dispatch(s, host, &v);
            if s.lose_response.remove(&op) {
                return vec![Reply::Drop];
            }
            if s.silent.contains(&op) {
                return Vec::new();
            }
            vec![Reply::Frame(resp.to_string())]
        }
        _ => Vec::new(),
    }
}

/// The acting mailbox of a request: `mailbox:<id>@<host>` or `human:<owner>@<host>`, bound here.
fn actor(s: &St, host: &str, req: &Value) -> Option<String> {
    let raw = req["actor"].as_str()?;
    let (kind, rest) = raw.split_once(':')?;
    let (target, at) = rest.split_once('@')?;
    if at != host {
        return None;
    }
    match kind {
        "mailbox" => s.mailbox(target).filter(|m| m.host.as_deref() == Some(host)).map(|m| m.id.clone()),
        "human" => s.mailboxes.iter().find(|m| m.human && m.owner == target && m.host.as_deref() == Some(host)).map(|m| m.id.clone()),
        _ => None,
    }
}

fn dispatch(s: &mut St, host: &str, req: &Value) -> Value {
    let id = req["request_id"].clone();
    let op = req["op"].as_str().unwrap_or_default().to_string();
    let body = &req["body"];
    let key = req["idempotency_key"].as_str().map(str::to_string);
    if let Some(k) = &key
        && let Some(stored) = s.idem.get(&(host.to_string(), op.clone(), k.clone()))
    {
        let mut r = stored.clone();
        r["request_id"] = id;
        return r;
    }
    let owner = s.hosts[host].owner.clone();
    let resp = match op.as_str() {
        "board.info" => ok(&id, json!({"name": "dakai", "owner": owner, "host": host, "mailbox_only": s.hosts[host].mailbox_only})),
        "host.migrate_preview" => ok(&id, json!({"rows": []})),
        "host.migrate_v2" => {
            s.hosts.get_mut(host).unwrap().mailbox_only = true;
            ok(&id, json!({"bindings": []}))
        }
        "mailbox.publish" => {
            let name = body["name"].as_str().unwrap_or_default().to_string();
            let human = body["human"].as_bool().unwrap_or(false);
            let take = body["take"].as_bool().unwrap_or(false);
            let existing = s.mailboxes.iter().position(|m| m.owner == owner && m.name == name);
            match existing {
                Some(i) if s.mailboxes[i].host.as_deref().is_some_and(|h| h != host) && !take => {
                    err(&id, "conflict", &format!("{owner}/{name} is published from another host; use --take"))
                }
                Some(i) => {
                    let old = s.mailboxes[i].host.clone();
                    let m = &mut s.mailboxes[i];
                    m.host = Some(host.into());
                    m.generation += 1;
                    m.human = human;
                    let result = json!({"name": name, "mailbox_id": m.id, "route_generation": m.generation});
                    if let Some(old) = old.filter(|o| o != host) {
                        s.abandon_stale();
                        let f = s.routes_frame(&old);
                        s.send_to(&old, f);
                    }
                    let f = s.routes_frame(host);
                    s.send_to(host, f);
                    ok(&id, result)
                }
                None => {
                    let mid = s.ulid();
                    s.mailboxes.push(Mailbox { id: mid.clone(), owner: owner.clone(), name: name.clone(), host: Some(host.into()), generation: 1, human });
                    let f = s.routes_frame(host);
                    s.send_to(host, f);
                    ok(&id, json!({"name": name, "mailbox_id": mid, "route_generation": 1}))
                }
            }
        }
        "mailbox.unpublish" => {
            let mid = body["mailbox_id"].as_str().unwrap_or_default();
            let expected = body["expected_generation"].as_u64().unwrap_or_default();
            match s.mailboxes.iter_mut().find(|m| m.id == mid) {
                Some(m) if m.host.as_deref() == Some(host) && m.generation == expected => {
                    m.host = None;
                    m.generation += 1;
                    let result = json!({"mailbox_id": m.id, "route_generation": m.generation});
                    s.abandon_stale();
                    let f = s.routes_frame(host);
                    s.send_to(host, f);
                    ok(&id, result)
                }
                _ => err(&id, "conflict", "the mailbox moved"),
            }
        }
        "mailbox.mine" => {
            let mine: Vec<Value> = s
                .mailboxes
                .iter()
                .filter(|m| m.host.as_deref() == Some(host))
                .map(|m| json!({"name": m.name, "mailbox_id": m.id, "route_generation": m.generation}))
                .collect();
            ok(&id, json!({"mailboxes": mine}))
        }
        "message.send" => match actor(s, host, req) {
            None => err(&id, "unauthenticated", "the actor is not a mailbox of this host"),
            Some(me) => {
                let to = body["to"]["agent"].as_str().unwrap_or_default();
                let (o, n) = to.split_once('/').map(|(o, n)| (o.to_string(), n.to_string())).unwrap_or((owner.clone(), to.to_string()));
                let target = s.mailboxes.iter().find(|m| m.owner == o && m.name == n && m.host.is_some()).map(|m| m.id.clone());
                match target {
                    None => err(&id, "not_found", &format!("recipient unavailable: {to}")),
                    Some(t) => {
                        let mid = s.ulid();
                        let reply_to = body["reply_to"].as_str().map(str::to_string);
                        s.messages.push(Msg { id: mid.clone(), from: me, to: t.clone(), text: body["text"].as_str().unwrap_or_default().into(), reply_to, read: false });
                        s.notes.insert(mid.clone(), Note { msg: mid.clone(), mailbox: t.clone(), status: "pending".into(), next_at: Instant::now(), current: None, raw: false });
                        ok(&id, json!({"message_id": mid, "seq": 1, "to": s.canonical(&t)}))
                    }
                }
            }
        },
        "message.get" | "message.read" | "inbox.list" | "receipt.list" | "agent.whoami" => match actor(s, host, req) {
            None => err(&id, "unauthenticated", "the actor is not a mailbox of this host"),
            Some(me) => read_op(s, &id, &op, body, &me),
        },
        "agent.list" => {
            let rows: Vec<Value> = s
                .mailboxes
                .iter()
                .filter(|m| m.host.is_some())
                .map(|m| json!({"name": format!("{}/{}", m.owner, m.name), "kind": "mailbox", "host": m.host}))
                .collect();
            ok(&id, json!({"agents": rows}))
        }
        other => err(&id, "bad_request", &format!("unknown op '{other}'")),
    };
    if let Some(k) = key
        && resp["ok"] == true
    {
        s.idem.insert((host.to_string(), op, k), resp.clone());
    }
    resp
}

fn view(s: &St, m: &Msg) -> Value {
    json!({"message_id": m.id, "place": "dm", "from": s.canonical(&m.from), "to": s.canonical(&m.to),
           "date": m.id, "reply_to": m.reply_to, "text": m.text, "attachments": [], "reactions": []})
}

fn read_op(s: &mut St, id: &Value, op: &str, body: &Value, me: &str) -> Value {
    match op {
        "agent.whoami" => ok(id, json!({"name": s.canonical(me), "address": format!("mailbox:{me}")})),
        "message.get" | "message.read" if body["id"].is_string() => {
            let mid = body["id"].as_str().unwrap();
            let Some(i) = s.messages.iter().position(|m| m.id == mid && (m.to == me || m.from == me)) else {
                return err(id, "not_found", &format!("no message {mid}"));
            };
            if op == "message.read" && s.messages[i].to == me {
                s.messages[i].read = true;
            }
            let v = view(s, &s.messages[i].clone());
            if op == "message.get" { ok(id, json!({"message": v})) } else { ok(id, json!({"messages": [v], "more": false})) }
        }
        "message.read" => {
            let mut out = Vec::new();
            for i in 0..s.messages.len() {
                if s.messages[i].to == me && !s.messages[i].read {
                    s.messages[i].read = true;
                    out.push(view(s, &s.messages[i].clone()));
                }
            }
            ok(id, json!({"messages": out, "more": false}))
        }
        "inbox.list" => {
            let items: Vec<Value> = s
                .messages
                .iter()
                .filter(|m| m.to == me && !m.read)
                .map(|m| json!({"kind": "dm", "message_id": m.id, "from": s.canonical(&m.from), "date": m.id, "preview": m.text}))
                .collect();
            ok(id, json!({"items": items, "next_cursor": null}))
        }
        "receipt.list" => {
            let rows: Vec<Value> = s
                .messages
                .iter()
                .filter(|m| m.from == me)
                .map(|m| json!({"message_id": m.id, "to": s.canonical(&m.to), "status": "sent", "delivery": s.notes[&m.id].status}))
                .collect();
            ok(id, json!({"receipts": rows, "next_cursor": null}))
        }
        _ => err(id, "bad_request", "unknown op"),
    }
}
