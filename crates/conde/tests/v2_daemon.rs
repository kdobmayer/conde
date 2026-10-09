//! The v2 daemon's acceptance gates (DESIGN-v2.md §11: G1 to G7, G11 to G17), in process:
//! two or more daemons, each with its own data, config and socket, against a scripted
//! shared board ([`fake_board`]) that stands in for the V1 server. Harness delivery is a
//! scripted notifier with the real notifier's contract (tokens as attempt ids, receipts by
//! token, mappings released by `forget`).

#[path = "support/fake_board.rs"]
mod fake_board;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use conde::address::{Address, Kind};
use conde::boards::{self, BoardConfig};
use conde::client::Client;
use conde::daemon::{Options, Running, Tuning};
use conde::notify::{Notifier, NotifyOutcome, Receipt, ReceiptStatus};
use conde::protocol::v2::op;
use conde::protocol::{ErrorCode, Request};
use conde::util::new_ulid;
use conde_remote::{Endpoint, Notify, NotifyFields, NotifyKind};
use fake_board::{Acks, FakeBoard};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- the scripted harness ---------------------------------------------------------

#[derive(Clone, Debug)]
enum CodexMode {
    Accept,
    Queue,
    Fail(String),
}

#[derive(Clone, Debug)]
struct Push {
    address: String,
    text: String,
    token: String,
}

#[derive(Default)]
struct HState {
    live: HashSet<String>,
    codex: HashMap<String, CodexMode>,
    claude_fail: Option<String>,
    pushes: Vec<Push>,
    receipts: VecDeque<Receipt>,
    /// Tokens whose receipts are still routed (notified Claude frames not forgotten).
    mappings: HashSet<String>,
    forgotten: Vec<String>,
    /// Answer every Claude push with this receipt at once.
    auto: Option<ReceiptStatus>,
}

/// A harness with the real notifier's contract: Claude frames are mapped by token until
/// `forget`, a receipt for an unmapped token is dropped; Codex answers by its mode.
#[derive(Clone, Default)]
struct Harness(Arc<Mutex<HState>>);

impl Harness {
    fn st(&self) -> std::sync::MutexGuard<'_, HState> {
        self.0.lock().unwrap()
    }

    fn live(&self, session: &str) {
        self.st().live.insert(session.into());
    }

    fn gone(&self, session: &str) {
        self.st().live.remove(session);
    }

    fn codex(&self, thread: &str, mode: CodexMode) {
        self.st().codex.insert(thread.into(), mode);
    }

    fn pushes_of(&self, message_id: &str) -> Vec<Push> {
        self.st().pushes.iter().filter(|p| p.text.contains(message_id)).cloned().collect()
    }

    fn wait_pushes(&self, message_id: &str, n: usize) -> Vec<Push> {
        wait_for(&format!("{n} push(es) of {message_id}"), || {
            let p = self.pushes_of(message_id);
            (p.len() >= n).then_some(p)
        })
    }

    fn receipt(&self, token: &str, status: ReceiptStatus) {
        self.st().receipts.push_back(Receipt { attempt_id: token.into(), status });
    }

    fn mapped(&self, token: &str) -> bool {
        self.st().mappings.contains(token)
    }
}

impl Notifier for Harness {
    fn notify(&mut self, address: &Address, text: &str, token: &str) -> NotifyOutcome {
        let mut st = self.st();
        st.pushes.push(Push { address: address.to_string(), text: text.into(), token: token.into() });
        match address.kind {
            Kind::Claude => {
                if !st.live.contains(&address.target) {
                    return NotifyOutcome::Failed(format!("no live Claude session {}", address.target));
                }
                if let Some(why) = st.claude_fail.clone() {
                    return NotifyOutcome::Failed(why);
                }
                st.mappings.insert(token.into());
                if let Some(status) = st.auto.clone() {
                    st.receipts.push_back(Receipt { attempt_id: token.into(), status });
                }
                NotifyOutcome::Accepted
            }
            Kind::Codex => match st.codex.get(&address.target).cloned().unwrap_or(CodexMode::Accept) {
                CodexMode::Accept => NotifyOutcome::Accepted,
                CodexMode::Queue => NotifyOutcome::Held,
                CodexMode::Fail(why) => NotifyOutcome::Failed(why),
            },
            _ => NotifyOutcome::Failed("no push transport".into()),
        }
    }

    fn poll_receipts(&mut self) -> Vec<Receipt> {
        let mut st = self.st();
        let all: Vec<Receipt> = st.receipts.drain(..).collect();
        all.into_iter().filter(|r| st.mappings.contains(&r.attempt_id)).collect()
    }

    fn forget(&mut self, token: &str) {
        let mut st = self.st();
        st.mappings.remove(token);
        st.forgotten.push(token.into());
    }

    fn is_live(&mut self, address: &Address) -> bool {
        match address.kind {
            Kind::Claude => self.st().live.contains(&address.target),
            _ => true,
        }
    }
}

// --- an in-process host -------------------------------------------------------------

type Reply = Result<Value, (ErrorCode, String)>;

struct Host {
    dir: tempfile::TempDir,
    user: String,
    host: String,
    harness: Harness,
    tuning: Tuning,
    daemon: Option<Running>,
}

fn fast() -> Tuning {
    Tuning {
        rescan: Duration::from_millis(100),
        backoff_max: Some(Duration::from_millis(200)),
        call_timeout: Duration::from_secs(5),
        outcome_ack_timeout: Some(Duration::from_millis(500)),
        idle_timeout: Some(Duration::from_secs(5)),
        ..Tuning::default()
    }
}

impl Host {
    fn new(user: &str, host: &str) -> Host {
        Host::tuned(user, host, fast())
    }

    fn tuned(user: &str, host: &str, tuning: Tuning) -> Host {
        let dir = tempfile::Builder::new().prefix("v2d").tempdir_in("/tmp").unwrap();
        Host { dir, user: user.into(), host: host.into(), harness: Harness::default(), tuning, daemon: None }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn sock(&self) -> PathBuf {
        self.dir.path().join("s.sock")
    }

    fn opts(&self) -> Options {
        let mut o = Options::new(self.dir.path().join("data"), self.sock());
        o.config_dir = Some(self.config());
        o.claude_config = Some(self.dir.path().join("claude"));
        o.interval = Duration::from_millis(50);
        o.retry_backoff = Some(Duration::from_millis(300));
        o.host = Some(self.host.clone());
        o.user = Some(self.user.clone());
        o.tuning = self.tuning.clone();
        o
    }

    fn start(&mut self) -> &mut Host {
        assert!(self.daemon.is_none());
        self.daemon = Some(Running::start(self.opts(), self.harness.clone()).unwrap());
        self
    }

    /// Ends the daemon as a process exit would: retained jobs, tokens and unacknowledged
    /// outcomes are gone.
    fn kill(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }

    fn core(&self) -> &Arc<conde::daemon::Core> {
        self.daemon.as_ref().unwrap().core()
    }

    fn request(&self, actor: &str, op: &str, body: Value, board: Option<&str>) -> Reply {
        let mut req = Request::new(new_ulid(), op, body);
        req.actor = Some(actor.into());
        req.idempotency_key = Some(new_ulid());
        req.board = board.map(str::to_string);
        let resp = Client::connect(&self.sock()).unwrap().call(&req).unwrap();
        match resp.error {
            None => Ok(resp.result.unwrap()),
            Some(e) => Err((e.code, e.message)),
        }
    }

    fn ok(&self, actor: &str, op: &str, body: Value, board: Option<&str>) -> Value {
        self.request(actor, op, body.clone(), board).unwrap_or_else(|e| panic!("{op} {body} as {actor}: {e:?}"))
    }

    fn human(&self) -> String {
        format!("human:{}", self.user)
    }

    /// Registers `name` from Claude session `session` (live).
    fn claude(&self, name: &str, session: &str) -> String {
        self.harness.live(session);
        let actor = format!("claude:{session}");
        self.ok(&actor, "agent.register", json!({"name": name}), None);
        actor
    }

    fn codex(&self, name: &str, thread: &str) -> String {
        let actor = format!("codex:{thread}");
        self.ok(&actor, "agent.register", json!({"name": name}), None);
        actor
    }

    /// Enrolls with `board` as alias `dakai` (before the daemon starts).
    fn enroll(&self, board: &FakeBoard) {
        let key = board.enroll(&self.user, &self.host);
        boards::store(
            &boards::path_in(&self.config(), "dakai"),
            "dakai",
            &BoardConfig { server: board.url(), host_id: self.host.clone(), owner: self.user.clone(), key },
        )
        .unwrap();
    }

    /// The one-time migration (nothing to publish) and the connection.
    fn migrate(&self, board: &FakeBoard) {
        self.ok(&self.human(), op::HOST_MIGRATE_V2, json!({"publish": []}), Some("dakai"));
        wait_for("the board connection", || (board.connected(&self.host) && self.status() == "connected").then_some(()));
        // the connection's own reconciliation request is done before the test counts frames
        wait_for("mailbox.mine", || board.capture(&self.host).iter().any(|f| f.contains("mailbox.mine")).then_some(()));
    }

    fn status(&self) -> String {
        self.core().board_status("dakai").unwrap_or_default()
    }

    fn publish(&self, name: &str) -> Value {
        self.ok(&self.human(), op::MAILBOX_PUBLISH, json!({"agent": name, "name": name}), Some("dakai"))
    }

    fn send(&self, actor: &str, to: &str, text: &str, board: Option<&str>) -> Reply {
        self.request(actor, "message.send", json!({"to": {"agent": to}, "text": text}), board)
    }

    fn doctor(&self) -> String {
        let r = self.ok(&self.human(), "doctor", json!({}), None);
        r["checks"].as_array().unwrap().iter().map(|c| format!("{} {}\n", c["ok"], c["text"].as_str().unwrap())).collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.kill();
    }
}

/// One enrolled, migrated, running host with agent `name` on Claude session `session`,
/// published on dakai.
fn published_host(board: &FakeBoard, user: &str, host: &str, name: &str, session: &str, tuning: Tuning) -> (Host, String) {
    let mut h = Host::tuned(user, host, tuning);
    h.enroll(board);
    h.start();
    h.migrate(board);
    let actor = h.claude(name, session);
    let p = h.publish(name);
    assert_eq!(p["state"], "published", "{p}");
    (h, actor)
}

fn id_of(sent: &Value) -> String {
    sent["message_id"].as_str().unwrap().to_string()
}

fn bare(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, b)| b)
}

#[test]
fn a_published_agent_receives_a_teammates_dm_and_settles_it() {
    let board = FakeBoard::start();
    let (a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    let relay = board.mailbox("balazs", "relay", "box-b");
    let msg = board.dm(&relay, "kdobmayer", "worker", "setup feedback");
    let push = a.harness.wait_pushes(&msg, 1).remove(0);
    assert_eq!(push.address, "claude:s-worker@homepc");
    assert_eq!(
        push.text,
        format!("conde [dakai]: new message dakai:{msg} from balazs/relay. Read it with: conde read dakai:{msg}")
    );
    a.harness.receipt(&push.token, ReceiptStatus::Delivered);
    wait_for("delivered", || (board.note(&msg).status == "delivered").then_some(()));
    let attempt = &board.attempts(&msg)[0];
    let reported: Vec<String> = board.outcomes_of("homepc", &attempt.id).into_iter().map(|o| o.1).collect();
    assert_eq!(reported.last().unwrap(), "delivered");
    // the token's mapping is released once the board's book took the final outcome
    wait_for("the mapping released", || (!a.harness.mapped(&push.token)).then_some(()));
}

// --- helpers for the gates -----------------------------------------------------------

fn dm_fields() -> Option<NotifyFields> {
    Some(NotifyFields { kind: NotifyKind::Dm, sender: "balazs/relay".into(), place: "dm".into(), badges: Vec::new(), member: true })
}

fn raw_notify(endpoint: Endpoint, generation: u64, fields: Option<NotifyFields>) -> Notify {
    Notify {
        notification_id: "n".into(),
        attempt_id: String::new(),
        recipient: "kdobmayer/worker".into(),
        binding_version: generation,
        endpoint,
        message_id: String::new(),
        text: String::new(),
        fields,
    }
}

fn mailbox_endpoint(id: &str) -> Endpoint {
    Endpoint::Mailbox { mailbox_id: id.into() }
}

/// The outcomes a host reported for an attempt, once at least one with `status` is there.
fn wait_outcome(board: &FakeBoard, host: &str, attempt: &str, status: &str) -> Vec<(u64, String, Option<String>)> {
    wait_for(&format!("a {status} outcome for {attempt}"), || {
        let o = board.outcomes_of(host, attempt);
        o.iter().any(|x| x.1 == status).then_some(o)
    })
}

fn wait_history(board: &FakeBoard, msg: &str, n: usize, history: &[&str]) {
    wait_for(&format!("attempt {n} of {msg} recorded as {history:?}"), || {
        let a = board.attempts(msg);
        (a.len() > n && a[n].history == history).then_some(())
    });
}

/// A local SQL connection to a host's board file (its own tables, beside the daemon).
fn sql(h: &Host) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(h.dir.path().join("data/conde.db")).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c
}

fn local_agent_id(h: &Host, name: &str) -> String {
    sql(h).query_row("SELECT agent_id FROM agents WHERE name_lower = ?1", [name], |r| r.get(0)).unwrap()
}

fn requests_since(board: &FakeBoard, host: &str, from: usize) -> Vec<String> {
    board.capture(host).into_iter().skip(from).filter(|f| f.contains("\"type\":\"request\"") || f.starts_with("POST")).collect()
}

// --- G1 ---------------------------------------------------------------------------------

/// G1 offline: with the board unreachable (stopped, then a blackholed route), local work never
/// waits on it; `inbox --board local` answers at once; `inbox` within 2 s with dakai unavailable.
#[test]
fn g1_offline_local_work_never_waits_on_a_board() {
    for case in ["stopped", "blackholed", "silent"] {
        let board = FakeBoard::start();
        let (mut a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
        if case == "silent" {
            // connected, but its answers never come
            board.silence("inbox.list");
        } else {
            a.kill();
            let server = if case == "blackholed" {
                // a non-routable address: the connect hangs until its timeout
                "ws://10.255.255.1:9".to_string()
            } else {
                // nothing listens on the port
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let url = format!("ws://{}", l.local_addr().unwrap());
                drop(l);
                url
            };
            let mut cfg = boards::load(&boards::path_in(&a.config(), "dakai")).unwrap();
            cfg.server = server;
            boards::forget_in(&a.config(), "dakai").unwrap();
            boards::store(&boards::path_in(&a.config(), "dakai"), "dakai", &cfg).unwrap();
            a.start();
        }
        let quick = |what: &str, t: Instant| assert!(t.elapsed() < Duration::from_millis(700), "{what} took {:?} ({case})", t.elapsed());

        let t = Instant::now();
        let reviewer = a.claude("reviewer", "s-reviewer");
        quick("register", t);
        let t = Instant::now();
        let id = id_of(&a.send(&worker, "reviewer", "offline hello", None).unwrap());
        quick("send", t);
        let t = Instant::now();
        let push = a.harness.wait_pushes(&id, 1).remove(0);
        quick("push", t);
        assert_eq!(push.address, "claude:s-reviewer@homepc");
        let t = Instant::now();
        let read = a.ok(&reviewer, "message.read", json!({"id": id}), None);
        quick("read", t);
        assert_eq!(read["messages"][0]["text"], "offline hello");

        let t = Instant::now();
        let local = a.ok(&worker, "inbox.list", json!({}), Some("local"));
        assert!(t.elapsed() < Duration::from_millis(300), "inbox --board local took {:?} ({case})", t.elapsed());
        assert!(local.get("boards").is_none(), "{local}");

        let t = Instant::now();
        let all = a.ok(&worker, "inbox.list", json!({}), None);
        assert!(t.elapsed() < Duration::from_millis(2500), "inbox took {:?} ({case})", t.elapsed());
        assert_eq!(all["boards"], json!([{"board": "local", "available": true}, {"board": "dakai", "available": false}]), "{all}");
        if case != "silent" {
            assert!(a.doctor().contains("dakai: unreachable") || a.doctor().contains("dakai: connecting"), "{}", a.doctor());
        }
    }
}

// --- G2 ---------------------------------------------------------------------------------

/// G2 nothing local on the wire: seeded session ids, thread ids, socket paths and unpublished
/// names never appear in anything the host sends, through enrollment, connect, publish,
/// failed deliveries and reconnects.
#[test]
fn g2_nothing_local_reaches_the_board() {
    let board = FakeBoard::start();
    let seeds = ["seed-sid-7f3a", "seed-sid-hidden-91c2", "seed-thread-44d1", "/tmp/seed-sock-a1b2", "seedhidden"];
    let no_leak = |phase: &str| {
        let capture = board.capture_all();
        for seed in seeds {
            assert!(!capture.contains(seed), "{seed} reached the board after {phase}:\n{capture}");
        }
    };
    let mut a = Host::new("kdobmayer", "homepc");
    a.enroll(&board);
    a.start();
    a.migrate(&board);
    // after enrollment alone the board has no row bound to the host and no endpoint
    assert!(board.mailboxes().iter().all(|m| m.host.as_deref() != Some("homepc")));
    assert!(board.endpoints("homepc").is_empty());
    no_leak("enrollment and connect");

    a.claude("worker", "seed-sid-7f3a");
    a.claude("seedhidden", "seed-sid-hidden-91c2");
    a.codex("coder", "seed-thread-44d1");
    a.publish("worker");
    a.publish("coder");
    let published: HashSet<String> =
        ["worker", "coder"].iter().map(|n| board.mailbox_named("kdobmayer", n).unwrap().id).collect();
    wait_for("the published mailboxes available", || (board.endpoints("homepc") == published).then_some(()));
    no_leak("publish");
    let relay = board.mailbox("balazs", "relay", "box-b");

    // no live session
    a.harness.gone("seed-sid-7f3a");
    let m1 = board.dm(&relay, "kdobmayer", "worker", "one");
    wait_for("m1 failed", || board.attempts(&m1).first().and_then(|x| x.status.clone()).filter(|s| s == "failed"));
    // a broken reply socket
    a.harness.live("seed-sid-7f3a");
    a.harness.st().claude_fail = Some("cannot deliver to /tmp/seed-sock-a1b2/inbox.sock: Connection refused (seed-sid-7f3a)".into());
    let m2 = board.dm(&relay, "kdobmayer", "worker", "two");
    wait_for("m2 failed", || board.attempts(&m2).first().and_then(|x| x.status.clone()).filter(|s| s == "failed"));
    // a Codex error
    a.harness.codex("seed-thread-44d1", CodexMode::Fail("thread seed-thread-44d1 not loaded (/tmp/seed-sock-a1b2/codex.sock)".into()));
    let m3 = board.dm(&relay, "kdobmayer", "coder", "three");
    wait_for("m3 failed", || board.attempts(&m3).first().and_then(|x| x.status.clone()).filter(|s| s == "failed"));
    no_leak("failed deliveries");
    // a reconnect
    board.drop_connection("homepc");
    wait_for("the reconnect", || (board.connections("homepc") >= 2).then_some(()));
    a.ok(&a.human(), "doctor", json!({}), None);
    no_leak("a reconnect");

    let capture = board.capture_all();
    for p in a.harness.st().pushes.iter() {
        assert!(!capture.contains(&p.token), "a delivery token reached the board");
    }
    let reasons: HashSet<Option<String>> = board.outcome_frames("homepc").into_iter().map(|o| o.3).collect();
    for r in &reasons {
        assert!(r.as_deref().is_none_or(|r| conde_remote::OutcomeReason::parse(r).is_some()), "{r:?} is outside the closed set");
    }
    assert!(reasons.contains(&Some("recipient offline".into())) && reasons.contains(&Some("transport error".into())), "{reasons:?}");
    // the details stay with the local doctor
    let doctor = a.doctor();
    assert!(doctor.contains("/tmp/seed-sock-a1b2/inbox.sock") && doctor.contains("seed-thread-44d1"), "{doctor}");
    a.kill();
}

// --- G3 ---------------------------------------------------------------------------------

/// G3 a local miss stays local: `send reviewer` with only dakai's kdobmayer/reviewer fails with
/// the local error, and the board receives nothing for it.
#[test]
fn g3_a_local_miss_stays_local() {
    let board = FakeBoard::start();
    let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    board.mailbox("kdobmayer", "reviewer", "laptop");
    let from = board.capture("homepc").len();
    let e = a.send(&worker, "reviewer", "hi", None).unwrap_err();
    assert_eq!(e.0, ErrorCode::NotFound);
    assert_eq!(e.1, "no agent 'reviewer' on the local board; shared boards are addressed with --board <name>");
    std::thread::sleep(Duration::from_millis(300));
    let after: Vec<String> = board.capture("homepc").into_iter().skip(from).collect();
    assert!(after.iter().all(|f| !f.contains("reviewer") && !f.contains("\"type\":\"request\"")), "{after:?}");
}

// --- G4 ---------------------------------------------------------------------------------

/// G4 identical names: local `reviewer` and dakai `kdobmayer/reviewer` on different sessions.
/// A plain send is local, `--board dakai` reaches the mailbox, a reply to a dakai id stays on
/// dakai, `--board dakai` with a bare id is refused. (MCP sends the same requests; its `board`
/// argument is the surface lane's.)
#[test]
fn g4_identical_names_resolve_by_board() {
    let board = FakeBoard::start();
    let (a, me) = published_host(&board, "kdobmayer", "homepc", "me", "s-me", fast());
    let local_reviewer = a.claude("reviewer", "s-local-rev");
    let (b, shared_reviewer) = published_host(&board, "kdobmayer", "laptop", "reviewer", "s-shared-rev", fast());

    let local_id = id_of(&a.send(&me, "reviewer", "local hello", None).unwrap());
    assert!(!local_id.contains(':'), "{local_id}");
    assert_eq!(a.harness.wait_pushes(&local_id, 1)[0].address, "claude:s-local-rev@homepc");

    let shared_id = id_of(&a.send(&me, "reviewer", "shared hello", Some("dakai")).unwrap());
    assert!(shared_id.starts_with("dakai:"), "{shared_id}");
    assert_eq!(b.harness.wait_pushes(bare(&shared_id), 1)[0].address, "claude:s-shared-rev@laptop");
    assert!(a.harness.pushes_of(bare(&shared_id)).is_empty());

    let reply = b.ok(
        &shared_reviewer,
        "message.send",
        json!({"to": {"agent": "kdobmayer/me"}, "text": "re", "reply_to": shared_id}),
        None,
    );
    let reply_id = id_of(&reply);
    assert!(reply_id.starts_with("dakai:"), "{reply}");
    let stored = board.messages().into_iter().find(|m| m.id == bare(&reply_id)).unwrap();
    assert_eq!(stored.reply_to.as_deref(), Some(bare(&shared_id)));
    assert_eq!(a.harness.wait_pushes(bare(&reply_id), 1)[0].address, "claude:s-me@homepc");

    let e = a.request(&me, "message.get", json!({"id": local_id}), Some("dakai")).unwrap_err();
    assert_eq!(e.1, format!("{local_id} is a local message; drop --board, or use the dakai: id"));
    let got = a.ok(&me, "message.get", json!({"id": reply_id}), None);
    assert_eq!((got["message"]["message_id"].as_str(), got["board"].as_str()), (Some(reply_id.as_str()), Some("dakai")));
    assert_eq!(got["message"]["reply_to"], json!(shared_id));
    let local = a.ok(&local_reviewer, "message.get", json!({"id": local_id}), None);
    assert_eq!(local["message"]["text"], "local hello");
}

// --- G5 ---------------------------------------------------------------------------------

/// G5 no injection without a live publication: every notify below is refused with
/// `recipient unavailable` and nothing is pushed.
#[test]
fn g5_no_injection_without_a_live_publication() {
    let board = FakeBoard::start();
    let (a, _worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    let worker = board.mailbox_named("kdobmayer", "worker").unwrap();
    a.claude("leaving", "s-leaving");
    a.publish("leaving");
    let leaving = board.mailbox_named("kdobmayer", "leaving").unwrap();
    a.claude("moving", "s-moving");
    a.publish("moving");
    let moving = board.mailbox_named("kdobmayer", "moving").unwrap();
    a.claude("draft", "s-draft");
    // a publication left `publishing` without its op, and one `unpublishing`
    let c = sql(&a);
    c.execute(
        "INSERT INTO publications(board, agent_id, shared_name, state, updated_at) VALUES ('dakai', ?1, 'draft', 'publishing', 't')",
        [local_agent_id(&a, "draft")],
    )
    .unwrap();
    c.execute("UPDATE publications SET state = 'unpublishing' WHERE shared_name = 'leaving'", []).unwrap();
    // moving is taken by another host of the owner; the routes frame reaches this host
    let taken_generation = board.take("kdobmayer", "moving", "laptop");
    wait_for("the routes applied", || a.doctor().contains("publication removed: moving").then_some(()));

    let pushes_before = a.harness.st().pushes.len();
    let cases = [
        ("a live local session", raw_notify(Endpoint::Claude { session_id: "s-worker".into() }, worker.generation, dm_fields())),
        ("an unknown mailbox", raw_notify(mailbox_endpoint("01UNKNOWNMAILBOX0000000000"), 1, dm_fields())),
        ("a publishing mailbox with no pending op", raw_notify(mailbox_endpoint("01DRAFTMAILBOX000000000000"), 1, dm_fields())),
        ("an unpublishing mailbox", raw_notify(mailbox_endpoint(&leaving.id), leaving.generation, dm_fields())),
        ("a stale route generation", raw_notify(mailbox_endpoint(&worker.id), worker.generation - 1, dm_fields())),
        ("a mailbox taken by another host", raw_notify(mailbox_endpoint(&moving.id), taken_generation, dm_fields())),
        ("a notify without the structured fields", raw_notify(mailbox_endpoint(&worker.id), worker.generation, None)),
    ];
    for (what, n) in cases {
        let attempt = board.push_raw("homepc", n);
        let outcomes = wait_outcome(&board, "homepc", &attempt, "refused");
        assert_eq!(outcomes.last().unwrap().2.as_deref(), Some("recipient unavailable"), "{what}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(a.harness.st().pushes.len(), pushes_before, "nothing was pushed");
}

// --- G6 ---------------------------------------------------------------------------------

/// G6 interruptions: a DM and its reply carry dakai: ids and `sent` shows both receipts; then a
/// dropped connection resends with the same event_seq (one push), a process death before the
/// report was sent re-attempts through the new instance (the message is not lost), and a
/// process death after a committed terminal outcome leaves it settled (no repeated push).
#[test]
fn g6_interruptions_keep_the_delivery_contract() {
    let board = FakeBoard::start();
    let (mut a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    let (b, relay) = published_host(&board, "balazs", "box-b", "relay", "s-relay", fast());
    let relay_mb = board.mailbox_named("balazs", "relay").unwrap().id;

    let ping = id_of(&b.send(&relay, "kdobmayer/worker", "ping", Some("dakai")).unwrap());
    assert!(ping.starts_with("dakai:"));
    let p = a.harness.wait_pushes(bare(&ping), 1).remove(0);
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    let inbox = a.ok(&worker, "inbox.list", json!({}), None);
    let row = inbox["items"].as_array().unwrap().iter().find(|i| i["message_id"] == json!(ping)).cloned();
    assert_eq!(row.map(|r| r["board"].clone()), Some(json!("dakai")), "{inbox}");
    let pong = id_of(&a.ok(&worker, "message.send", json!({"to": {"agent": "balazs/relay"}, "text": "pong", "reply_to": ping}), None));
    assert!(pong.starts_with("dakai:"));
    // the reply left as the worker's mailbox, never as its session
    let worker_mailbox = board.mailbox_named("kdobmayer", "worker").unwrap().id;
    let sends: Vec<Value> = board
        .capture("homepc")
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .filter(|v| v["op"] == "message.send")
        .collect();
    assert!(!sends.is_empty() && sends.iter().all(|v| v["actor"] == json!(format!("mailbox:{worker_mailbox}@homepc")) && v.get("from").is_none()), "{sends:?}");
    let p = b.harness.wait_pushes(bare(&pong), 1).remove(0);
    b.harness.receipt(&p.token, ReceiptStatus::Delivered);
    let receipt = |h: &Host, actor: &str, id: &str| {
        let r = h.ok(actor, "receipt.list", json!({}), None);
        r["receipts"].as_array().unwrap().iter().find(|x| x["message_id"] == json!(id)).map(|x| (x["delivery"].clone(), x["board"].clone()))
    };
    wait_for("both receipts delivered", || {
        (receipt(&b, &relay, &ping) == Some((json!("delivered"), json!("dakai")))
            && receipt(&a, &worker, &pong) == Some((json!("delivered"), json!("dakai"))))
        .then_some(())
    });

    // 1. the connection drops, the process stays: the same event_seq is resent, settled once
    board.set_acks("homepc", Acks::LoseAck);
    let m = board.dm(&relay_mb, "kdobmayer", "worker", "case 1");
    a.harness.wait_pushes(&m, 1);
    wait_history(&board, &m, 0, &["accepted"]);
    let attempt = board.attempts(&m)[0].id.clone();
    board.set_acks("homepc", Acks::Normal);
    board.drop_connection("homepc");
    let resent = wait_for("the resend", || {
        let o = board.outcomes_of("homepc", &attempt);
        (o.len() >= 2).then_some(o)
    });
    assert!(resent.iter().all(|o| o.0 == resent[0].0 && o.1 == "accepted"), "{resent:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(board.attempts(&m)[0].history, ["accepted"], "settled once");
    assert_eq!(a.harness.pushes_of(&m).len(), 1, "no repeated push");

    // 2. killed after the report was taken into the client's memory, before it was sent
    board.set_acks("homepc", Acks::Stall);
    let m = board.dm(&relay_mb, "kdobmayer", "worker", "case 2");
    a.harness.wait_pushes(&m, 1);
    let first = wait_for("the first attempt", || board.attempts(&m).first().cloned());
    wait_outcome(&board, "homepc", &first.id, "accepted");
    a.kill();
    board.set_acks("homepc", Acks::Normal);
    a.start();
    let again = a.harness.wait_pushes(&m, 2);
    assert_eq!(board.attempts(&m)[0].history, ["abandoned"], "the old instance's attempt is abandoned");
    a.harness.receipt(&again[1].token, ReceiptStatus::Delivered);
    wait_for("case 2 delivered", || (board.note(&m).status == "delivered").then_some(()));

    // 3. killed after the board committed a terminal outcome whose ack was lost
    board.set_acks("homepc", Acks::LoseAck);
    let m = board.dm(&relay_mb, "kdobmayer", "worker", "case 3");
    let p = a.harness.wait_pushes(&m, 1).remove(0);
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    wait_for("the terminal outcome committed", || board.attempts(&m)[0].history.contains(&"delivered".to_string()).then_some(()));
    a.kill();
    board.set_acks("homepc", Acks::Normal);
    a.start();
    wait_for("reconnected", || (a.status() == "connected").then_some(()));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(a.harness.pushes_of(&m).len(), 1, "no repeated push");
    assert_eq!(board.attempts(&m).len(), 1);
    assert_eq!(board.note(&m).status, "delivered");
}

// --- G7 ---------------------------------------------------------------------------------

/// G7 publish first: an unpublished agent's `send --board dakai` gets the publish error and
/// nothing is sent.
#[test]
fn g7_an_unpublished_agent_sends_nothing() {
    let board = FakeBoard::start();
    let mut a = Host::new("kdobmayer", "homepc");
    a.enroll(&board);
    a.start();
    a.migrate(&board);
    let worker = a.claude("worker", "s-worker");
    let from = board.capture("homepc").len();
    let e = a.send(&worker, "balazs/relay", "hi", Some("dakai")).unwrap_err();
    assert_eq!(e.0, ErrorCode::Forbidden);
    assert_eq!(e.1, "worker is not published on dakai. Publication is set by your human: conde publish worker --board dakai");
    std::thread::sleep(Duration::from_millis(200));
    let sent = requests_since(&board, "homepc", from);
    assert!(sent.is_empty(), "{sent:?}");
}

// --- G11 --------------------------------------------------------------------------------

fn slots(h: &Host, origin: &str) -> conde::daemon::OriginStats {
    h.core().origin_stats().into_iter().find(|s| s.origin == origin).unwrap()
}

/// G11 a stalled board: the board stops acknowledging; more notifications than the window are
/// due; a held attempt's late delivered receipt arrives while the queue and the client's book
/// are full. Local delivery keeps its latency, memory stays within the window and the book,
/// routes frames are applied throughout, and once acks resume everything settles without a
/// restart.
#[test]
fn g11_a_stalled_board_only_keeps_its_window_full() {
    let tuning = Tuning { window: 4, max_unacked_outcomes: Some(4), ..fast() };
    let board = FakeBoard::start();
    let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", tuning);
    let reviewer = a.claude("reviewer", "s-reviewer");
    a.claude("spare", "s-spare");
    a.publish("spare");
    let relay = board.mailbox("balazs", "relay", "box-b");
    board.set_acks("homepc", Acks::Stall);
    let msgs: Vec<String> = (0..10).map(|i| board.dm(&relay, "kdobmayer", "worker", &format!("m{i}"))).collect();
    let pushed = |h: &Host| msgs.iter().filter(|m| !h.harness.pushes_of(m).is_empty()).count();
    wait_for("the window pushed", || (pushed(&a) == 4).then_some(()));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(pushed(&a), 4, "nothing beyond the window");
    assert_eq!(board.dispatching("homepc"), 4);

    // a held attempt's late delivered receipt while the book is full
    let first = msgs.iter().find_map(|m| a.harness.pushes_of(m).first().cloned()).unwrap();
    a.harness.receipt(&first.token, ReceiptStatus::Held);
    std::thread::sleep(Duration::from_millis(200));
    a.harness.receipt(&first.token, ReceiptStatus::Delivered);
    wait_for("the receipt retained in the outcome map", || (slots(&a, "dakai").outcomes >= 1).then_some(()));
    let s = slots(&a, "dakai");
    assert!(s.queued <= 4 && s.outcomes <= 4 + s.budget && s.used <= 4, "{s:?}");
    assert!(a.doctor().contains("book 4"), "{}", a.doctor());

    // local delivery at normal latency
    let t = Instant::now();
    let local = id_of(&a.send(&reviewer, "worker", "local during the stall", None).unwrap());
    a.harness.wait_pushes(&local, 1);
    assert!(t.elapsed() < Duration::from_millis(800), "local push took {:?}", t.elapsed());

    // routes frames are applied throughout
    board.take("kdobmayer", "spare", "laptop");
    wait_for("the routes applied during the stall", || a.doctor().contains("publication removed: spare").then_some(()));

    // acks resume: the board recovers by itself
    board.set_acks("homepc", Acks::Normal);
    a.harness.st().auto = Some(ReceiptStatus::Delivered);
    for m in &msgs {
        for p in a.harness.pushes_of(m) {
            if p.token != first.token {
                a.harness.receipt(&p.token, ReceiptStatus::Delivered);
            }
        }
    }
    wait_for("every notification delivered", || msgs.iter().all(|m| board.note(m).status == "delivered").then_some(()));
    let late = msgs.iter().find(|m| a.harness.pushes_of(m).iter().any(|p| p.token == first.token)).unwrap();
    assert!(board.attempts(late)[0].history.contains(&"delivered".to_string()), "the late receipt is settled");
    assert_eq!(a.harness.pushes_of(late).len(), 1);
    let _ = worker;
}

/// G11b retention under healthy acks (R = 2): a held attempt keeps its mapping; first-outcome
/// traffic continues beyond R; the origin pauses at R while local delivery continues; the late
/// terminal receipt is settled and the origin resumes, without a restart.
#[test]
fn g11b_a_held_attempt_pauses_only_its_origin() {
    let tuning = Tuning { receipt_budget: 2, window: 8, ..fast() };
    let board = FakeBoard::start();
    let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", tuning);
    a.codex("coder", "t-coder");
    a.publish("coder");
    a.claude("reviewer", "s-reviewer");
    let relay = board.mailbox("balazs", "relay", "box-b");

    let held = board.dm(&relay, "kdobmayer", "worker", "held");
    let p_held = a.harness.wait_pushes(&held, 1).remove(0);
    wait_history(&board, &held, 0, &["accepted"]);
    a.harness.receipt(&p_held.token, ReceiptStatus::Held);
    wait_history(&board, &held, 0, &["accepted", "held"]);

    for i in 0..5 {
        let m = board.dm(&relay, "kdobmayer", "coder", &format!("codex {i}"));
        wait_for("codex reach", || board.reached(&m).then_some(()));
    }
    let open = board.dm(&relay, "kdobmayer", "worker", "open");
    let p_open = a.harness.wait_pushes(&open, 1).remove(0);
    wait_for("both slots used", || (slots(&a, "dakai").used == 2).then_some(()));

    let waiting = board.dm(&relay, "kdobmayer", "coder", "waits");
    wait_for("the job queued", || (slots(&a, "dakai").queued == 1).then_some(()));
    std::thread::sleep(Duration::from_millis(500));
    assert!(a.harness.pushes_of(&waiting).is_empty(), "the origin pauses at R");
    assert!(a.doctor().contains("slots: dakai 2/2 used"), "{}", a.doctor());

    let local = id_of(&a.send(&worker, "reviewer", "local continues", None).unwrap());
    a.harness.wait_pushes(&local, 1);
    assert!(a.harness.mapped(&p_held.token) && a.harness.mapped(&p_open.token), "no unresolved mapping is evicted");

    a.harness.receipt(&p_held.token, ReceiptStatus::Delivered);
    wait_for("the late receipt settled", || (board.note(&held).status == "delivered").then_some(()));
    wait_for("the origin resumes", || board.reached(&waiting).then_some(()));
    wait_for("the held mapping released", || (!a.harness.mapped(&p_held.token)).then_some(()));
    assert!(a.harness.mapped(&p_open.token));
}

// --- G12 --------------------------------------------------------------------------------

/// G12 a lost board: a revoked credential, and separately a fence by another instance, stop the
/// connection only; the daemon and local delivery keep running and doctor names the state.
#[test]
fn g12_a_lost_board_stops_only_its_connection() {
    for case in ["revoked", "fenced"] {
        let board = FakeBoard::start();
        let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
        let reviewer = a.claude("reviewer", "s-reviewer");
        match case {
            "revoked" => board.revoke("homepc"),
            _ => board.fence("homepc"),
        }
        wait_for("the connection stopped", || a.status().starts_with("stopped").then_some(()));
        let doctor = a.doctor();
        let named = match case {
            "revoked" => "dakai: stopped: the board rejected this host's credential",
            _ => "dakai: stopped: fenced by a newer bridge",
        };
        assert!(doctor.contains(named), "{doctor}");
        let id = id_of(&a.send(&reviewer, "worker", "still local", None).unwrap());
        assert_eq!(a.harness.wait_pushes(&id, 1)[0].address, "claude:s-worker@homepc");
        a.ok(&worker, "inbox.list", json!({}), None);
    }
}

// --- G13 --------------------------------------------------------------------------------

/// G13 session replacement without a disconnected interval: the held attempt on the old session
/// is abandoned and re-attempted at the new one, a delayed receipt from the old session is
/// ignored, and the mailbox's availability does not flap.
#[test]
fn g13_a_session_replacement_reattempts_at_the_new_session() {
    let board = FakeBoard::start();
    let (a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-old", fast());
    let mailbox = board.mailbox_named("kdobmayer", "worker").unwrap().id;
    wait_for("the mailbox available", || board.endpoints("homepc").contains(&mailbox).then_some(()));
    let relay = board.mailbox("balazs", "relay", "box-b");
    let m = board.dm(&relay, "kdobmayer", "worker", "held on the old session");
    let old = a.harness.wait_pushes(&m, 1).remove(0);
    wait_history(&board, &m, 0, &["accepted"]);
    a.harness.receipt(&old.token, ReceiptStatus::Held);
    wait_history(&board, &m, 0, &["accepted", "held"]);

    a.claude("worker", "s-new");
    let pushes = a.harness.wait_pushes(&m, 2);
    assert_eq!(pushes[1].address, "claude:s-new@homepc");
    let first = &board.attempts(&m)[0];
    let reported = board.outcomes_of("homepc", &first.id);
    assert_eq!(reported.last().map(|o| (o.1.as_str(), o.2.as_deref())), Some(("abandoned", Some("recipient session replaced"))));

    a.harness.receipt(&old.token, ReceiptStatus::Delivered);
    std::thread::sleep(Duration::from_millis(400));
    assert!(board.outcomes_of("homepc", &first.id).iter().all(|o| o.1 != "delivered"), "the old session's receipt is ignored");
    a.harness.receipt(&pushes[1].token, ReceiptStatus::Delivered);
    wait_for("delivered at the new session", || (board.note(&m).status == "delivered").then_some(()));

    let flapped = board
        .capture("homepc")
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .any(|v| v["type"] == "endpoints" && v["removed"].as_array().is_some_and(|r| r.iter().any(|e| e["mailbox_id"] == json!(mailbox))));
    assert!(!flapped, "the mailbox never left the endpoint set");
    assert!(board.endpoints("homepc").contains(&mailbox));
}

// --- G14 --------------------------------------------------------------------------------

/// G14 takeover with a queued notify: host A has a job queued and not injected; host B
/// publishes the mailbox with --take. A applies the routes frame and drops the job; the board
/// re-attempts it at B. The job A injected before the frame is the documented race.
#[test]
fn g14_a_takeover_moves_the_queued_job_to_the_new_host() {
    let tuning = Tuning { receipt_budget: 1, ..fast() };
    let board = FakeBoard::start();
    let (a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-a", tuning);
    let mut b = Host::new("kdobmayer", "laptop");
    b.enroll(&board);
    b.start();
    b.migrate(&board);
    b.claude("worker", "s-b");
    let relay = board.mailbox("balazs", "relay", "box-b");

    let injected = board.dm(&relay, "kdobmayer", "worker", "injected before the move");
    a.harness.wait_pushes(&injected, 1);
    let queued = board.dm(&relay, "kdobmayer", "worker", "queued at the move");
    wait_for("the job queued on A", || (slots(&a, "dakai").queued == 1).then_some(()));
    assert!(a.harness.pushes_of(&queued).is_empty());

    let took = b.ok(&b.human(), op::MAILBOX_PUBLISH, json!({"agent": "worker", "name": "worker", "take": true}), Some("dakai"));
    assert_eq!(took["state"], "published", "{took}");
    wait_for("A applied the routes frame", || a.doctor().contains("publication removed: worker").then_some(()));
    let at_b = b.harness.wait_pushes(&queued, 1).remove(0);
    assert_eq!(at_b.address, "claude:s-b@laptop");
    std::thread::sleep(Duration::from_millis(300));
    assert!(a.harness.pushes_of(&queued).is_empty(), "A never injects the queued job");
    // the documented race: injected on A before the frame, re-attempted at B
    b.harness.wait_pushes(&injected, 1);
    assert_eq!(a.harness.pushes_of(&injected).len(), 1);
    b.harness.receipt(&at_b.token, ReceiptStatus::Delivered);
    wait_for("delivered at B", || (board.note(&queued).status == "delivered").then_some(()));
    assert_eq!(slots(&a, "dakai").queued, 0);
}

// --- G15 --------------------------------------------------------------------------------

/// G15 unpublish and re-publish: after unpublish a remote send gets `recipient unavailable` and
/// an old-generation notify is refused; re-publishing makes a new generation and messages flow;
/// an unpublish replayed after the mailbox moved to another host does not unbind it.
#[test]
fn g15_unpublish_and_republish() {
    let board = FakeBoard::start();
    let (mut a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    let (b, relay) = published_host(&board, "balazs", "box-b", "relay", "s-relay", fast());
    let before = board.mailbox_named("kdobmayer", "worker").unwrap();

    let gone = a.ok(&a.human(), op::MAILBOX_UNPUBLISH, json!({"agent": "worker"}), Some("dakai"));
    assert_eq!(gone["state"], "unpublished");
    let e = b.send(&relay, "kdobmayer/worker", "anyone?", Some("dakai")).unwrap_err();
    assert!(e.1.contains("recipient unavailable"), "{e:?}");
    let stale = board.push_raw("homepc", raw_notify(mailbox_endpoint(&before.id), before.generation, dm_fields()));
    let o = wait_outcome(&board, "homepc", &stale, "refused");
    assert_eq!(o.last().unwrap().2.as_deref(), Some("recipient unavailable"));

    let again = a.publish("worker");
    assert_eq!(again["mailbox_id"], json!(before.id));
    assert!(again["route_generation"].as_u64().unwrap() > before.generation + 1, "{again}");
    let id = id_of(&b.send(&relay, "kdobmayer/worker", "back", Some("dakai")).unwrap());
    let p = a.harness.wait_pushes(bare(&id), 1).remove(0);
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    wait_for("delivered again", || (board.note(bare(&id)).status == "delivered").then_some(()));

    // an unpublish that was pending when the process died, replayed after the move
    let current = board.mailbox_named("kdobmayer", "worker").unwrap();
    a.kill();
    let c = sql(&a);
    let agent = local_agent_id(&a, "worker");
    c.execute("UPDATE publications SET state = 'unpublishing' WHERE agent_id = ?1", [&agent]).unwrap();
    c.execute(
        "INSERT INTO board_ops(op_id, board, kind, agent_id, request_json, state, created_at, updated_at)
         VALUES ('01REPLAYEDUNPUBLISH0000000', 'dakai', 'unpublish', ?1, ?2, 'pending', 't', 't')",
        rusqlite::params![agent, json!({"mailbox_id": current.id, "expected_generation": current.generation}).to_string()],
    )
    .unwrap();
    board.take("kdobmayer", "worker", "laptop");
    a.start();
    wait_for("the replayed unpublish finished", || {
        let state: String = sql(&a).query_row("SELECT state FROM board_ops WHERE op_id = '01REPLAYEDUNPUBLISH0000000'", [], |r| r.get(0)).unwrap();
        (state == "done").then_some(())
    });
    assert_eq!(board.mailbox_named("kdobmayer", "worker").unwrap().host.as_deref(), Some("laptop"), "the new route is untouched");
    let rows: i64 = sql(&a).query_row("SELECT count(*) FROM publications WHERE agent_id = ?1", [&agent], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

// --- G16 --------------------------------------------------------------------------------

/// G16 operation recovery: a publish whose answer is lost and whose process dies before the local
/// write resumes with the same key and ends published; a notify in the gap is retried, not
/// refused; a second publish or an unpublish while one is pending is refused.
#[test]
fn g16_a_publish_resumes_after_a_crash() {
    let tuning = Tuning { call_timeout: Duration::from_secs(1), ..fast() };
    let board = FakeBoard::start();
    let mut a = Host::tuned("kdobmayer", "homepc", tuning);
    a.enroll(&board);
    a.start();
    a.migrate(&board);
    a.claude("worker", "s-worker");
    let (_b, _relay) = published_host(&board, "balazs", "box-b", "relay", "s-relay", fast());
    let relay = board.mailbox_named("balazs", "relay").unwrap().id;

    board.silence("mailbox.publish");
    let pending = a.publish("worker");
    assert_eq!(pending["state"], "publishing", "{pending}");
    let op_id = pending["op_id"].as_str().unwrap().to_string();

    let m = board.dm(&relay, "kdobmayer", "worker", "in the gap");
    let first = wait_for("the gap attempt", || board.attempts(&m).first().cloned());
    let o = wait_outcome(&board, "homepc", &first.id, "failed");
    assert_eq!(o.last().unwrap().2.as_deref(), Some("recipient unavailable"), "retried, not refused");
    assert!(board.outcomes_of("homepc", &first.id).iter().all(|o| o.1 != "refused"));

    let second = a.request(&a.human(), op::MAILBOX_PUBLISH, json!({"agent": "worker", "name": "worker"}), Some("dakai")).unwrap_err();
    assert_eq!(second.0, ErrorCode::Conflict);
    assert!(second.1.contains(&format!("pending publish on dakai (op {op_id})")), "{second:?}");
    let unpublish = a.request(&a.human(), op::MAILBOX_UNPUBLISH, json!({"agent": "worker"}), Some("dakai")).unwrap_err();
    assert_eq!(unpublish.0, ErrorCode::Conflict);
    assert!(unpublish.1.contains(&op_id), "{unpublish:?}");

    a.kill();
    board.answer("mailbox.publish");
    a.start();
    wait_for("published after the resume", || {
        let state: String = sql(&a).query_row("SELECT state FROM publications WHERE shared_name = 'worker'", [], |r| r.get(0)).unwrap();
        (state == "published").then_some(())
    });
    let keys: Vec<String> = board
        .capture("homepc")
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .filter(|v| v["op"] == "mailbox.publish")
        .map(|v| v["idempotency_key"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(keys.len() >= 2 && keys.iter().all(|k| *k == op_id), "{keys:?}");
    let p = wait_for("the retried notify pushed", || a.harness.pushes_of(&m).first().cloned());
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    wait_for("delivered", || (board.note(&m).status == "delivered").then_some(()));
}

// --- G17 --------------------------------------------------------------------------------

/// G17 both harness flows over a shared mailbox: Claude reports accepted, held, delivered; Codex
/// reports delivered on injection and held when queued; fleet "reached" is right for both.
#[test]
fn g17_claude_and_codex_outcomes_are_normalized() {
    let board = FakeBoard::start();
    let (a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    a.codex("coder", "t-coder");
    a.publish("coder");
    a.codex("queuer", "t-queue");
    a.publish("queuer");
    a.harness.codex("t-queue", CodexMode::Queue);
    let relay = board.mailbox("balazs", "relay", "box-b");

    let m = board.dm(&relay, "kdobmayer", "worker", "claude");
    let p = a.harness.wait_pushes(&m, 1).remove(0);
    wait_history(&board, &m, 0, &["accepted"]);
    assert!(!board.reached(&m));
    a.harness.receipt(&p.token, ReceiptStatus::Held);
    wait_history(&board, &m, 0, &["accepted", "held"]);
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    wait_history(&board, &m, 0, &["accepted", "held", "delivered"]);
    assert!(board.reached(&m));

    let c = board.dm(&relay, "kdobmayer", "coder", "codex");
    wait_history(&board, &c, 0, &["delivered"]);
    assert!(board.reached(&c));

    let q = board.dm(&relay, "kdobmayer", "queuer", "codex queued");
    wait_history(&board, &q, 0, &["held"]);
    assert!(!board.reached(&q));
    for msg in [&m, &c, &q] {
        let a0 = &board.attempts(msg)[0];
        assert!(board.outcomes_of("homepc", &a0.id).iter().all(|o| o.2.is_none()), "no reason on success");
    }
}

// --- publication rules, the v1 credential, reconciliation on connect ------------------

/// Publishing again under the same name is a no-op; a different `--as` while published is
/// refused; publication is the human's; an unpublishing publication is not usable outbound;
/// an explicit shared board on a registry op is refused; whoami shows both names.
#[test]
fn publication_rules() {
    let board = FakeBoard::start();
    let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    assert_eq!(a.publish("worker")["unchanged"], true);
    let other = a.request(&a.human(), op::MAILBOX_PUBLISH, json!({"agent": "worker", "name": "other"}), Some("dakai")).unwrap_err();
    assert_eq!(other, (ErrorCode::Conflict, "worker is published on dakai as worker; unpublish first".to_string()));
    let by_agent = a.request(&worker, op::MAILBOX_PUBLISH, json!({"agent": "worker", "name": "worker"}), Some("dakai")).unwrap_err();
    assert_eq!(by_agent.0, ErrorCode::Forbidden);
    for (op, verb) in [("agent.register", "register"), ("agent.rename", "rename"), ("agent.pin", "pin"), ("agent.unregister", "unregister")] {
        let e = a.request(&worker, op, json!({"name": "worker"}), Some("dakai")).unwrap_err();
        assert_eq!(e.1, format!("{verb} is local; a shared address is conde publish <name> --board dakai"));
    }
    let lane = a.claude("lane", "s-lane");
    let p = a.ok(&a.human(), op::MAILBOX_PUBLISH, json!({"agent": "lane", "name": "lane-homepc"}), Some("dakai"));
    assert_eq!(p["name"], "lane-homepc");
    let who = a.ok(&lane, "agent.whoami", json!({}), None);
    assert_eq!(who["name"], "kdobmayer/lane");
    assert_eq!(who["publications"], json!([{"board": "dakai", "name": "kdobmayer/lane-homepc", "state": "published"}]));
    assert_eq!(who["boards"], json!(["dakai"]));
    // the agent may shrink its own exposure
    assert_eq!(a.ok(&lane, op::MAILBOX_UNPUBLISH, json!({"agent": "lane"}), Some("dakai"))["state"], "unpublished");
    sql(&a).execute("UPDATE publications SET state = 'unpublishing' WHERE shared_name = 'worker'", []).unwrap();
    let e = a.send(&worker, "balazs/relay", "x", Some("dakai")).unwrap_err();
    assert_eq!(e.1, conde::surface::publication_pending("worker", "dakai", "unpublishing"));
}

/// The v1 credential file moves to `boards/<board name>.json` with the account the board names;
/// until the migration is confirmed no hello is sent.
#[test]
fn the_v1_credential_moves_to_its_board_file() {
    let board = FakeBoard::start();
    let key = board.enroll("kdobmayer", "homepc");
    let mut a = Host::new("kdobmayer", "homepc");
    let legacy = boards::legacy_path(&a.config());
    conde::credentials::store(&legacy, &conde::credentials::Credentials { server: board.url(), host_id: "homepc".into(), key }).unwrap();
    a.start();
    wait_for("the credential moved", || (boards::path_in(&a.config(), "dakai").exists() && !legacy.exists()).then_some(()));
    let cfg = boards::load(&boards::path_in(&a.config(), "dakai")).unwrap();
    assert_eq!((cfg.owner.as_str(), cfg.host_id.as_str()), ("kdobmayer", "homepc"));
    wait_for("the board listed", || (a.status() == "not migrated").then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(board.connections("homepc"), 0, "no hello before the migration");
    a.migrate(&board);
}

/// Reconciliation on connect (`mailbox.mine`, with no routes frame on connect) removes a
/// publication the board moved while the daemon was down and adopts a newer generation.
#[test]
fn reconciliation_on_connect_uses_mailbox_mine() {
    let board = FakeBoard::start();
    board.routes_on_connect(false);
    let (mut a, _) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    a.claude("spare", "s-spare");
    a.publish("spare");
    a.kill();
    board.take("kdobmayer", "worker", "laptop");
    let generation = board.take("kdobmayer", "spare", "homepc");
    a.start();
    wait_for("reconciled", || a.doctor().contains("publication removed: worker").then_some(()));
    let stored: i64 = sql(&a).query_row("SELECT route_generation FROM publications WHERE shared_name = 'spare'", [], |r| r.get(0)).unwrap();
    assert_eq!(stored as u64, generation);
    let relay = board.mailbox("balazs", "relay", "box-b");
    let m = board.dm(&relay, "kdobmayer", "spare", "at the new generation");
    let p = a.harness.wait_pushes(&m, 1).remove(0);
    a.harness.receipt(&p.token, ReceiptStatus::Delivered);
    wait_for("delivered", || (board.note(&m).status == "delivered").then_some(()));
}

/// An aggregate `read` takes a board's unread messages and marks them read there; `--board`
/// narrows any aggregate view to one board.
#[test]
fn aggregate_views_merge_and_narrow() {
    let board = FakeBoard::start();
    let (a, worker) = published_host(&board, "kdobmayer", "homepc", "worker", "s-worker", fast());
    let reviewer = a.claude("reviewer", "s-reviewer");
    let relay = board.mailbox("balazs", "relay", "box-b");
    let shared = board.dm(&relay, "kdobmayer", "worker", "from dakai");
    let local = id_of(&a.send(&reviewer, "worker", "from local", None).unwrap());
    let only = |board: Option<&str>| {
        let r = a.ok(&worker, "inbox.list", json!({}), board);
        r["items"].as_array().unwrap().iter().map(|i| (i["board"].as_str().unwrap_or_default().to_string(), i["message_id"].as_str().unwrap().to_string())).collect::<Vec<_>>()
    };
    assert_eq!(only(Some("dakai")), [("dakai".to_string(), format!("dakai:{shared}"))]);
    assert_eq!(only(Some("local")), [(String::new(), local.clone())]);
    let both = only(None);
    assert!(both.contains(&("dakai".to_string(), format!("dakai:{shared}"))) && both.contains(&("local".to_string(), local.clone())), "{both:?}");
    let read = a.ok(&worker, "message.read", json!({}), None);
    let ids: Vec<&str> = read["messages"].as_array().unwrap().iter().map(|m| m["message_id"].as_str().unwrap()).collect();
    assert!(ids.contains(&local.as_str()) && ids.contains(&format!("dakai:{shared}").as_str()), "{read}");
    assert!(board.messages().iter().find(|m| m.id == shared).unwrap().read);
    assert!(only(None).is_empty());

    // a board that lists but does not answer the read within the bound: its messages are shown
    // unconfirmed (the read may land later), never dropped unseen
    let late = board.dm(&relay, "kdobmayer", "worker", "late read");
    board.silence("message.read");
    let t = Instant::now();
    let read = a.ok(&worker, "message.read", json!({}), None);
    assert!(t.elapsed() < Duration::from_millis(2500), "read took {:?}", t.elapsed());
    let row = read["messages"].as_array().unwrap().iter().find(|m| m["message_id"] == json!(format!("dakai:{late}"))).cloned();
    assert_eq!(row.map(|r| (r["unconfirmed"].clone(), r["text"].clone())), Some((json!(true), json!("late read"))), "{read}");
    assert!(read["boards"].as_array().unwrap().iter().any(|b| b["board"] == "dakai" && b["unconfirmed"] == true), "{read}");
}
