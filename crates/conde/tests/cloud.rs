//! Cloud mode end to end on 127.0.0.1: an in-process `conde` server over a
//! temp board, and bridges as real `conde daemon --server` processes, each
//! with its own host id, enrolled through `conde enroll`.

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

    /// Enroll `h` and start its bridge daemon.
    fn bridge(&self, h: &mut Host, log: &str, sock: &std::path::Path, extra: &[&str]) -> u32 {
        let url = self.url();
        let mut args = vec!["--server", url.as_str()];
        args.extend_from_slice(extra);
        h.daemon(log, sock, &args)
    }

    fn enroll(&self, h: &Host) {
        let key = self.key(&h.user, &h.host);
        let out = h.conde(None, &[], &["enroll", "--server", &self.url(), "--host", &h.host, "--key-stdin"], Some(&key));
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let path = h.root.join("config/conde/credentials");
        assert_eq!(conde::credentials::mode(&path), Some(0o600));
        assert!(!String::from_utf8_lossy(&out.stdout).contains(&key), "the key is never echoed");
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

fn test_config() -> ServerConfig {
    ServerConfig { poll_interval: Duration::from_millis(100), ..ServerConfig::default() }
}

fn queued_id(out: &str) -> String {
    out.strip_prefix("queued ").and_then(|s| s.split(' ').next()).expect(out).to_string()
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
    cloud.bridge(&mut a, "bridge.log", &sa, &[]);
    cloud.bridge(&mut b, "bridge.log", &sb, &[]);

    // registration, identity and names are the server's, host stamped from the key
    assert_eq!(a.ok(Some("codex"), ALICE, &["register", "alice"]), "registered ann/alice -> codex:t-alice@host-a\n");
    assert_eq!(b.ok(Some("claude"), CAROL, &["register", "carol"]), "registered ben/carol -> claude:s-carol@host-b\n");
    let out = b.conde(Some("claude"), CAROL, &["register", "ann/alice"], None);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in your namespace"), "{out:?}");

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "ben/carol", "hello across hosts"]));
    let f = carol.wait_frames(&id, 1);
    assert!(f[0].text.contains(&format!("conde: new message {id} from ann/alice")), "{}", f[0].text);
    assert!(!f[0].raw.contains("from-mode"));
    a.wait_delivery("codex", ALICE, &id, "delivered");
    let read = b.ok(Some("claude"), CAROL, &["read", &id]);
    assert!(read.contains("From: ann/alice\nTo: ben/carol\n") && read.ends_with("\nhello across hosts\n"), "{read}");
    assert!(a.sent_line("codex", ALICE, &id).contains(&format!("{id}  ben/carol  read 20")));

    // doctor on a bridge host: the server's checks plus the bridge's own
    // (exit 1: this codex caller has no app-server socket, which doctor flags)
    let out = a.conde(Some("codex"), ALICE, &["doctor"], None);
    let doctor = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(out.status.code(), Some(1), "{doctor}");
    assert!(doctor.contains("ISSUE   codex: no app-server control socket"), "{doctor}");
    assert!(doctor.contains("ok      bridge: connected to ws://"), "{doctor}");
    assert!(doctor.contains("credentials: ") && doctor.contains("mode 600"), "{doctor}");
    assert!(doctor.contains("ok      database: ok"), "{doctor}");
    // the remote API has no --from override and no bridge ops
    let out = a.conde(None, &[], &["--from", "alice", "whoami"], None);
    assert!(!out.status.success());

    // a revoked key cannot connect again
    let revoked = cloud.d.with_tx(|c| conde::ops::hosts::revoke(c, Some("host-a"), None, "now")).unwrap();
    assert_eq!(revoked.len(), 1);
    use conde_remote::Authenticator;
    let creds = conde::credentials::load(&a.root.join("config/conde/credentials")).unwrap();
    assert!(conde::cloud::Board::new(cloud.d.clone()).authenticate(&creds.key).is_none());
}

#[test]
fn restarted_bridge_fences_the_old_instance_and_resends_once() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    cloud.enroll(&a);
    cloud.enroll(&b);
    let sa = a.sock();
    cloud.bridge(&mut a, "bridge.log", &sa, &[]);
    let old_sock = b.root.join("run/old.sock");
    let old = cloud.bridge(&mut b, "old.log", &old_sock, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok_at(&old_sock, Some("claude"), CAROL, &["register", "carol"]);

    // first frame: held at the recipient (mid-flight); the re-sent frame: delivered
    carol.plan(vec![vec![(ms(0), "held", json!({}))], vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "ben/carol", "survive a restart"]));
    a.wait_delivery("codex", ALICE, &id, "held");

    // a new bridge instance for host-b takes over while the old one still runs
    let new_sock = b.root.join("run/new.sock");
    let new = cloud.bridge(&mut b, "new.log", &new_sock, &[]);
    let status = b.wait_exit(old);
    assert!(!status.success());
    assert!(b.log("old.log").contains("fenced"), "{}", b.log("old.log"));
    assert!(!old_sock.exists(), "the fenced daemon removed its socket");

    let f = carol.wait_frames(&id, 2);
    assert_ne!(f[0].msg_id, f[1].msg_id);
    assert!(f[1].from.ends_with(&format!("/cc/{new}.sock")), "re-sent by the new instance: {}", f[1].from);
    a.wait_delivery("codex", ALICE, &id, "delivered");
    std::thread::sleep(ms(1500));
    assert_eq!(carol.frames_for(&id).len(), 2, "re-sent exactly once");
    let attempts = cloud.attempts(&id);
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!((attempts[0].1.as_str(), attempts[0].2.as_deref()), ("abandoned", Some("bridge instance gone")));
    assert_eq!(attempts[1].1, "delivered");
    assert_ne!(attempts[0].0, attempts[1].0);
}

#[test]
fn an_idle_bridge_stays_connected_through_pings() {
    let ping = Duration::from_millis(300);
    let cloud = Cloud::start(ServerConfig { ping_interval: ping, max_missed_pongs: 2, ..test_config() });
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    // The bridge drops a connection that is silent for 1 s; only pings arrive.
    cloud.bridge(&mut a, "bridge.log", &sa, &["--idle-timeout", "1"]);
    let first = a.ok(None, &[], &["doctor"]);
    assert!(first.contains("bridge: connected"), "{first}");
    // A second daemon on the same socket refuses before it ever says hello.
    let file = std::fs::File::create(a.root.join("second.log")).unwrap();
    let second = a.env(&mut std::process::Command::new(BIN)).args(["daemon", "--server", &cloud.url()]).stderr(file).status();
    assert!(!second.unwrap().success());
    assert!(a.log("second.log").contains("another conde daemon is listening"), "{}", a.log("second.log"));
    std::thread::sleep(ping * 7);
    let doctor = a.ok(None, &[], &["doctor"]);
    assert!(doctor.contains("1 connection(s) since start"), "{doctor}\n{}", a.log("bridge.log"));
    assert!(!a.log("bridge.log").contains("connection lost"), "{}", a.log("bridge.log"));
}

#[test]
fn enrollment_is_bound_to_its_server_and_admin_is_human_only() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    // a different server never gets this host's key
    let file = std::fs::File::create(a.root.join("other.log")).unwrap();
    let out = a
        .env(&mut std::process::Command::new(BIN))
        .args(["daemon", "--server", "ws://127.0.0.2:9"])
        .stderr(file)
        .status()
        .unwrap();
    assert!(!out.success());
    assert!(a.log("other.log").contains("was enrolled with"), "{}", a.log("other.log"));
    // an agent's shell cannot mint keys
    let out = a.conde(Some("codex"), ALICE, &["admin", "key", "list", "--data", "/nonexistent"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("human only"));
    // healthz answers from the store
    let rt = &cloud.rt;
    let body = rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(cloud.addr).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).await.unwrap();
        raw
    });
    assert!(body.starts_with("HTTP/1.1 200"), "{body}");
    let _ = &mut a;
}

#[test]
fn a_session_stop_reported_by_endpoints_abandons_its_hold() {
    let cloud = Cloud::start(test_config());
    cloud.d.set_retry_ms(500);
    let mut a = Host::new("ann", "host-a");
    let mut b = Host::new("ben", "host-b");
    let carol = FakeClaude::start(&b, "s-carol");
    cloud.enroll(&a);
    cloud.enroll(&b);
    let (sa, sb) = (a.sock(), b.sock());
    cloud.bridge(&mut a, "bridge.log", &sa, &[]);
    cloud.bridge(&mut b, "bridge.log", &sb, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    carol.plan(vec![vec![(ms(0), "held", json!({}))]]);
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "ben/carol", "held, then the session ends"]));
    a.wait_delivery("codex", ALICE, &id, "held");

    // The session ends: the bridge's next rescan sends an endpoints diff, the
    // server abandons the hold, and the re-attempt finds no session.
    let session_file = b.root.join(format!("claude/sessions/{}.json", std::process::id()));
    let saved = std::fs::read(&session_file).unwrap();
    std::fs::remove_file(&session_file).unwrap();
    a.wait_delivery("codex", ALICE, &id, "retrying after failed (no live Claude session s-carol)");
    let attempts = cloud.attempts(&id);
    assert_eq!((attempts[0].1.as_str(), attempts[0].2.as_deref()), ("abandoned", Some("recipient session gone")));
    // It comes back: the retry reaches it.
    std::fs::write(&session_file, saved).unwrap();
    carol.wait_frames(&id, 2);
    a.wait_delivery("codex", ALICE, &id, "notified");
}

/// Two server replicas on one Postgres board (CONDE_TEST_PG): a message sent
/// through one reaches a bridge connected to the other by LISTEN/NOTIFY (the
/// poll is far too slow to explain it), and a bridge instance fenced through
/// one replica is closed by the other.
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
    let sa = a.sock();
    two.bridge(&mut a, "bridge.log", &sa, &[]);
    let old_sock = b.root.join("run/old.sock");
    let old = one.bridge(&mut b, "old.log", &old_sock, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    b.ok_at(&old_sock, Some("claude"), CAROL, &["register", "carol"]);
    std::thread::sleep(ms(500));

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let sent = std::time::Instant::now();
    let id = queued_id(&a.ok(Some("codex"), ALICE, &["send", "ben/carol", "across replicas"]));
    carol.wait_frames(&id, 1);
    assert!(sent.elapsed() < Duration::from_secs(10), "woken, not polled: {:?}", sent.elapsed());
    a.wait_delivery("codex", ALICE, &id, "delivered");

    // host-b's bridge moves to replica two; replica one closes the old instance
    two.enroll(&b);
    let new_sock = b.root.join("run/new.sock");
    two.bridge(&mut b, "new.log", &new_sock, &[]);
    let status = b.wait_exit(old);
    assert!(!status.success());
    assert!(b.log("old.log").contains("fenced"), "{}", b.log("old.log"));
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
