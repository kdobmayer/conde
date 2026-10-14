//! The shared board end to end on 127.0.0.1: an in-process `conde` server over a temp board,
//! and hosts as real `conde daemon` processes, each with its own host id, enrolled through
//! `conde enroll`, switched to v2 through `conde boards --migrate`, its agents published by
//! `conde publish`. One host stays on v1 (`conde daemon --v1-bridge`) to show that v1 and
//! v2 hosts keep exchanging messages.
#![cfg(unix)] // drives the daemon over a unix socket

mod support;
#[path = "pg/mod.rs"]
mod pg;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use conde::dispatch::Dispatcher;
use conde_remote::ServerConfig;
use serde_json::json;
use support::*;

const ALICE: &[(&str, &str)] = &[("CODEX_THREAD_ID", "t-alice")];
const CAROL: &[(&str, &str)] = &[("CLAUDE_CODE_SESSION_ID", "s-carol")];

struct Cloud {
    rt: tokio::runtime::Runtime,
    d: Arc<Dispatcher>,
    addr: SocketAddr,
    dir: std::path::PathBuf,
}

impl Drop for Cloud {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Cloud {
    /// A server on a fresh board: Postgres when CONDE_TEST_PG is set, else SQLite.
    fn start(config: ServerConfig) -> Cloud {
        Cloud::replica(pg::fresh_database().as_deref(), config)
    }

    /// A server on `database` (Postgres; another replica of the same board), or
    /// on a fresh SQLite board when None.
    fn replica(database: Option<&str>, config: ServerConfig) -> Cloud {
        let dir = std::env::temp_dir().join(format!("cde-srv-{}", &conde::util::new_ulid()[14..]));
        std::fs::create_dir_all(&dir).unwrap();
        let d = Arc::new(match database {
            Some(url) => Dispatcher::connect(url, conde::store::postgres::PgOpen::SERVER).unwrap(),
            None => Dispatcher::open(&dir.join("conde.db")).unwrap(),
        });
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let handle = rt
            .block_on(conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config))
            .unwrap();
        let addr = handle.local_addr();
        std::mem::forget(handle); // runs until the runtime drops
        Cloud { rt, d, addr, dir }
    }

    fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Mint a key for `host` (what `conde admin key create` stores).
    fn key(&self, user: &str, host: &str) -> String {
        let key = format!("conde_test_{}", conde::util::new_ulid());
        let digest = conde_remote::token_digest(&key);
        self.d
            .with_tx(|c| conde::ops::hosts::insert_key(c, user, host, &digest, "test", "2026-10-09T00:00:00.000Z"))
            .unwrap();
        key
    }

    /// `conde enroll` with a fresh key: `boards/shared.json`, pinned to the account.
    fn enroll(&self, h: &Host) {
        let key = self.key(&h.user, &h.host);
        let out = h.conde(None, &[], &["enroll", "--server", &self.url(), "--host", &h.host, "--key-stdin"], Some(&key));
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!String::from_utf8_lossy(&out.stdout).contains(&key), "the key is never echoed");
        assert_eq!(conde::credentials::mode(&h.root.join("config/conde/boards/shared.json")), Some(0o600));
    }

    /// Start `h`'s daemon on `sock` and switch the host to v2 (nothing bound yet, so nothing
    /// to review); returns the daemon's pid once its board connection is up. The switch and
    /// publication are the human's: a test running under an agent harness cannot derive the
    /// human from its own process tree, so these go to the daemon socket as the human, as
    /// `conde setup` and `conde publish` send them.
    fn join(&self, h: &mut Host, log: &str, sock: &std::path::Path, extra: &[&str]) -> u32 {
        let pid = h.daemon(log, sock, extra);
        let m = human_call(h, sock, "host.migrate_v2", json!({"publish": []}));
        assert!(m["bindings"].is_array(), "{m}\n{}", h.log(log));
        wait_for("the shared board connected", || {
            let d = h.conde(None, &[("CONDE_SOCK", sock.to_str().unwrap())], &["doctor"], None);
            String::from_utf8_lossy(&d.stdout).contains("ok      shared: connected").then_some(())
        });
        pid
    }

    /// Publish `name` on the shared board (the human).
    fn publish(&self, h: &Host, sock: &std::path::Path, name: &str) {
        let r = human_call(h, sock, "mailbox.publish", json!({"agent": name, "name": name}));
        assert_eq!((r["state"].as_str(), r["name"].as_str()), (Some("published"), Some(name)), "{r}");
    }

    /// The v1 bridge of a host that is not migrated (`conde daemon --v1-bridge`).
    fn bridge_v1(&self, h: &mut Host, log: &str, sock: &std::path::Path) -> u32 {
        let url = self.url();
        h.daemon(log, sock, &["--v1-bridge", url.as_str()])
    }

    /// The v1 credential the v1 bridge reads.
    fn enroll_v1(&self, h: &Host) {
        let key = self.key(&h.user, &h.host);
        let path = h.root.join("config/conde/credentials");
        let creds = conde::credentials::Credentials { server: self.url(), host_id: h.host.clone(), key };
        conde::credentials::store(&path, &creds).unwrap();
        assert_eq!(conde::credentials::mode(&path), Some(0o600));
    }

    /// (bridge instance, status, reason) of the message's attempts, oldest first.
    fn attempts(&self, message_id: &str) -> Vec<(String, String, Option<String>)> {
        let prefix = format!("{message_id}.");
        self.d
            .with_conn(|tx| tx.attempts())
            .unwrap()
            .into_iter()
            .filter(|a| a.notification_id.starts_with(&prefix))
            .map(|a| (a.bridge_instance, a.status, a.reason))
            .collect()
    }
}

/// One request to `h`'s daemon as its human, on the shared board.
fn human_call(h: &Host, sock: &std::path::Path, op: &str, body: serde_json::Value) -> serde_json::Value {
    let mut req = conde::protocol::Request::new(conde::util::new_ulid(), op, body);
    req.actor = Some(format!("human:{}", h.user));
    req.idempotency_key = Some(conde::util::new_ulid());
    req.board = Some("shared".into());
    let resp = conde::client::Client::connect(sock).unwrap().call(&req).unwrap();
    assert!(resp.ok, "{op}: {:?}", resp.error);
    resp.result.unwrap()
}

fn test_config() -> ServerConfig {
    ServerConfig { poll_interval: Duration::from_millis(100), ..ServerConfig::default() }
}

fn queued_id(out: &str) -> String {
    out.strip_prefix("queued ").and_then(|s| s.split(' ').next()).expect(out).to_string()
}


/// The delivery state the sender's `conde sent` shows for `id`, once it is `state`.
fn wait_sent(h: &Host, harness: &str, env: &[(&str, &str)], id: &str, state: &str) -> String {
    wait_for(&format!("delivery {state} of {id}"), || {
        let out = h.ok(Some(harness), env, &["sent", "--board", "shared"]);
        out.lines().find(|l| l.contains(id) && l.contains(&format!("delivery: {state}"))).map(str::to_string)
    })
}

#[test]
fn two_hosts_exchange_a_dm_through_the_server() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    cloud.enroll(&a);
    cloud.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    cloud.join(&mut b, "daemon.log", &sb, &[]);

    // names are registered on the local board; publication puts them on the shared one
    let out = a.ok(Some("codex"), ALICE, &["register", "alice"]);
    assert!(out.starts_with("registered ann/alice on the local board (codex:t-alice@host-a)"), "{out}");
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    let out = b.conde(Some("claude"), CAROL, &["register", "ann/alice"], None);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in your namespace"), "{out:?}");
    let out = a.conde(Some("codex"), ALICE, &["send", "--board", "shared", "ben/carol", "too early"], None);
    assert!(String::from_utf8_lossy(&out.stderr).contains("alice is not published on shared"), "{out:?}");
    cloud.publish(&a, &sa, "alice");
    cloud.publish(&b, &sb, "carol");
    // after enrollment and publication, the server binds mailboxes only, never a session
    let bound = cloud.d.with_conn(|tx| tx.agents_on_host("host-b")).unwrap();
    assert!(!format!("{bound:?}").contains("s-carol"), "{bound:?}");

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "--board", "shared", "ben/carol", "hello across hosts"]));
    assert!(id.starts_with("shared:"), "{id}");
    let f = carol.wait_frames(&id, 1);
    assert!(
        f[0].text.contains(&format!("\nconde [shared]: new message {id} from ann/alice. Read it with: conde read {id}\n")),
        "{}",
        f[0].text
    );
    wait_sent(&a, "codex", ALICE, &id, "delivered");
    let read = b.ok(Some("claude"), CAROL, &["read", &id]);
    assert!(read.contains("From: ann/alice\nTo: ben/carol\n") && read.ends_with("\nhello across hosts\n"), "{read}");

    // the reply inherits the shared board from the id
    let reply = queued_id(&b.ok(Some("claude"), CAROL, &["send", "ann/alice", "back", "--reply-to", &id]));
    assert!(reply.starts_with("shared:"), "{reply}");

    // doctor on a v2 host: the local board, the shared connection, the delivery slots
    let doctor = String::from_utf8_lossy(&a.conde(Some("codex"), ALICE, &["doctor"], None).stdout).to_string();
    assert!(doctor.contains("ok      shared: connected to ws://"), "{doctor}");
    assert!(doctor.contains("credentials: ") && doctor.contains("boards/shared.json mode 600"), "{doctor}");
    assert!(doctor.contains("ok      database: ok"), "{doctor}");
    assert!(doctor.contains("ok      slots: shared "), "{doctor}");

    // a revoked key stops the board connection only; the local board keeps working
    let revoked = cloud.d.with_tx(|c| conde::ops::hosts::revoke(c, Some("host-a"), None, "now")).unwrap();
    assert_eq!(revoked.len(), 1);
    wait_for("the connection stopped", || {
        let d = String::from_utf8_lossy(&a.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: the board rejected this host's credential").then_some(())
    });
    a.ok(Some("codex"), ALICE, &["send", "alice", "still local"]);
}

#[test]
fn a_restarted_daemon_is_a_new_instance_and_resends_once() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    cloud.enroll(&a);
    cloud.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    let old = cloud.join(&mut b, "old.log", &sb, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&a, &sa, "alice");
    cloud.publish(&b, &sb, "carol");

    // first frame: held at the recipient (mid-flight); the re-sent frame: delivered
    carol.plan(vec![vec![(ms(0), "held", json!({}))], vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "--board", "shared", "ben/carol", "survive a restart"]));
    wait_sent(&a, "codex", ALICE, &id, "held");

    // the process ends; the next one is a new bridge instance and the hold is re-attempted
    b.terminate(old);
    b.daemon("new.log", &sb, &[]);
    let f = carol.wait_frames(&id, 2);
    assert_ne!(f[0].msg_id, f[1].msg_id, "a new token, never the board's attempt id");
    wait_sent(&a, "codex", ALICE, &id, "delivered");
    std::thread::sleep(ms(1500));
    assert_eq!(carol.frames_for(&id).len(), 2, "re-sent exactly once");
    let attempts = cloud.attempts(id.trim_start_matches("shared:"));
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!(attempts[0].1, "abandoned");
    assert_eq!(attempts[1].1, "delivered");
    assert_ne!(attempts[0].0, attempts[1].0);
}

#[test]
fn a_second_daemon_with_the_hosts_key_fences_only_the_board_connection() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    // another daemon of the same host (its own socket and board file) takes the connection over
    let other_sock = a.root.join("run/other.sock");
    a.daemon("other.log", &other_sock, &[]);
    wait_for("the first connection fenced", || {
        let d = String::from_utf8_lossy(&a.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: fenced by a newer bridge").then_some(())
    });
    // the first daemon still serves its local board
    a.ok(Some("codex"), ALICE, &["send", "alice", "local after the fence"]);
    assert!(a.ok(Some("codex"), ALICE, &["inbox", "--board", "local"]).contains("local after the fence"));
}

#[test]
fn an_idle_connection_stays_up_through_pings() {
    let ping = Duration::from_millis(300);
    let cloud = Cloud::start(ServerConfig { ping_interval: ping, max_missed_pongs: 2, ..test_config() });
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    // The board connection drops after 1 s of silence; only pings arrive.
    cloud.join(&mut a, "daemon.log", &sa, &["--idle-timeout", "1"]);
    // A second daemon on the same socket refuses before it ever says hello.
    let file = std::fs::File::create(a.root.join("second.log")).unwrap();
    let second = a.env(&mut std::process::Command::new(BIN)).args(["daemon"]).stderr(file).status();
    assert!(!second.unwrap().success());
    assert!(a.log("second.log").contains("another conde daemon is listening"), "{}", a.log("second.log"));
    std::thread::sleep(ping * 7);
    let doctor = a.ok(None, &[], &["doctor"]);
    assert!(doctor.contains("1 connection(s) since start"), "{doctor}\n{}", a.log("daemon.log"));
    assert!(!a.log("daemon.log").contains("connection lost"), "{}", a.log("daemon.log"));
}

#[test]
fn admin_is_human_only_and_healthz_answers() {
    let cloud = Cloud::start(test_config());
    let a = Host::new("ann", "host-a");
    // an agent's shell cannot mint keys
    let out = a.conde(Some("codex"), ALICE, &["admin", "key", "list", "--data", "/nonexistent"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("human only"));
    // healthz answers from the store
    let body = cloud.rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(cloud.addr).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).await.unwrap();
        raw
    });
    assert!(body.starts_with("HTTP/1.1 200"), "{body}");
}

#[test]
fn a_session_that_stops_gives_its_hold_back_to_the_board() {
    let cloud = Cloud::start(test_config());
    cloud.d.set_retry_ms(500);
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    cloud.enroll(&a);
    cloud.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    cloud.join(&mut b, "daemon.log", &sb, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&a, &sa, "alice");
    cloud.publish(&b, &sb, "carol");
    carol.plan(vec![vec![(ms(0), "held", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "--board", "shared", "ben/carol", "held, then the session ends"]));
    wait_sent(&a, "codex", ALICE, &id, "held");

    // The session ends: the hold is abandoned (recipient offline, no session detail on the
    // wire) and the re-attempt finds no session.
    let session_file = b.root.join(format!("claude/sessions/{}.json", std::process::id()));
    let saved = std::fs::read(&session_file).unwrap();
    std::fs::remove_file(&session_file).unwrap();
    wait_sent(&a, "codex", ALICE, &id, "retrying after failed (recipient offline)");
    let attempts = cloud.attempts(id.trim_start_matches("shared:"));
    assert_eq!((attempts[0].1.as_str(), attempts[0].2.as_deref()), ("abandoned", Some("recipient offline")));
    // It comes back: the retry reaches it.
    std::fs::write(&session_file, saved).unwrap();
    carol.wait_frames(&id, 2);
    wait_sent(&a, "codex", ALICE, &id, "notified");
}

/// A host that has not migrated keeps its v1 bridge (`--v1-bridge`, its names on the server)
/// and exchanges messages with a migrated host's published agent, both ways.
#[test]
fn a_v1_host_and_a_v2_host_exchange_messages() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    let dave = FakeClaude::start(&a, "s-dave");
    cloud.enroll_v1(&a);
    cloud.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    cloud.bridge_v1(&mut a, "bridge.log", &sa);
    cloud.join(&mut b, "daemon.log", &sb, &[]);
    let out = a.ok(Some("claude"), &[("CLAUDE_CODE_SESSION_ID", "s-dave")], &["register", "dave"]);
    assert!(out.contains("ann/dave"), "{out}");
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&b, &sb, "carol");

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let to_v2 = queued_id(&a.ok(Some("claude"), &[("CLAUDE_CODE_SESSION_ID", "s-dave")], &["send", "ben/carol", "from v1"]));
    let f = carol.wait_frames(&format!("shared:{to_v2}"), 1);
    assert!(f[0].text.contains("from ann/dave"), "{}", f[0].text);

    dave.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let to_v1 = queued_id(&b.ok(Some("claude"), CAROL, &["send", "--board", "shared", "ann/dave", "from v2"]));
    let f = dave.wait_frames(to_v1.trim_start_matches("shared:"), 1);
    assert!(f[0].text.contains("\nconde: new message "), "the v1 bridge injects the board's text: {}", f[0].text);
}

/// Two server replicas on one Postgres board (CONDE_TEST_PG): a message sent
/// through one reaches a host connected to the other by LISTEN/NOTIFY (the
/// poll is far too slow to explain it), and an instance fenced through one
/// replica is closed by the other.
#[test]
fn replicas_wake_and_fence_each_other() {
    let Some(db) = pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to run the two-replica test");
        return;
    };
    let slow = ServerConfig { poll_interval: Duration::from_secs(30), ..ServerConfig::default() };
    let (one, two) = (Cloud::replica(Some(&db), slow.clone()), Cloud::replica(Some(&db), slow));
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    two.enroll(&a);
    one.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    two.join(&mut a, "daemon.log", &sa, &[]);
    one.join(&mut b, "daemon.log", &sb, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    two.publish(&a, &sa, "alice");
    one.publish(&b, &sb, "carol");
    std::thread::sleep(ms(500));

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let sent = std::time::Instant::now();
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "--board", "shared", "ben/carol", "across replicas"]));
    carol.wait_frames(&id, 1);
    assert!(sent.elapsed() < Duration::from_secs(10), "woken, not polled: {:?}", sent.elapsed());
    wait_sent(&a, "codex", ALICE, &id, "delivered");

    // a second daemon of host-b on replica two; replica one closes the first instance
    let other_sock = b.root.join("run/other.sock");
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(b.root.join("config/conde/boards/shared.json")).unwrap()).unwrap();
    cfg["server"] = json!(two.url());
    std::fs::write(b.root.join("config/conde/boards/shared.json"), cfg.to_string()).unwrap();
    b.daemon("other.log", &other_sock, &[]);
    wait_for("the first instance fenced", || {
        let d = String::from_utf8_lossy(&b.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: fenced by a newer bridge").then_some(())
    });
}

/// `conde server` on Postgres (CONDE_TEST_PG): ready on /healthz, and SIGTERM
/// closes it cleanly, the board's last handle dropped without a panic.
#[test]
fn server_on_postgres_stops_cleanly_on_sigterm() {
    let Some(db) = pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to run conde server on Postgres");
        return;
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = std::process::Command::new(BIN)
        .args(["server", "--listen", &format!("127.0.0.1:{port}")])
        .env("DATABASE_URL", &db)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    wait_for("healthz 200", || {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").ok()?;
        let mut raw = String::new();
        s.read_to_string(&mut raw).ok()?;
        raw.starts_with("HTTP/1.1 200").then_some(())
    });
    // SAFETY: kill(2) on the child this test spawned.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let out = child.wait_with_output().unwrap();
    let log = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "exit {:?}\n{log}", out.status);
    assert!(!log.contains("panicked"), "{log}");
}

/// `conde enroll` against the real server: `board.info` over `/v1/rpc` with the host key
/// names the alias (the server's advertised name) and the pinned account; nothing is bound.
#[test]
fn enroll_takes_the_alias_and_account_from_board_info() {
    let cloud = Cloud::start(test_config());
    let h = Host::new("ann", "host-a");
    let key = cloud.key("ann", "host-a");
    let out = h.conde(None, &[], &["enroll", "--server", &cloud.url(), "--key-stdin"], Some(&key));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with(&format!("enrolled host host-a with {} as board shared (account ann)", cloud.url())), "{stdout}");
    assert!(!stdout.contains(&key), "the key is never echoed");
    let path = h.root.join("config/conde/boards/shared.json");
    assert_eq!(conde::credentials::mode(&path), Some(0o600));
    let stored: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!((stored["owner"].as_str(), stored["host_id"].as_str()), (Some("ann"), Some("host-a")));
    let bound = cloud.d.with_conn(|tx| tx.agents_on_host("host-a")).unwrap();
    assert!(bound.is_empty(), "enrollment binds nothing on the server: {bound:?}");
}
