//! The shared board end to end on 127.0.0.1: an in-process `conde` server over a temp board,
//! and hosts as real `conde daemon` processes, each with its own host id, enrolled through
//! `conde enroll`, switched to v2 through `conde boards --migrate`, its agents published by
//! `conde publish`. One host stays on v1 (`conde daemon --v1-bridge`) to show that v1 and
//! v2 hosts keep exchanging messages. The cases with a Claude Code session fake its unix-socket
//! inbox, so they run on Unix only.
// on Windows the helpers of those cases stand unused
#![cfg_attr(windows, allow(dead_code))]

#[path = "pg/mod.rs"]
mod pg;
mod support;

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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt
            .block_on(conde::cloud::start(
                d.clone(),
                "127.0.0.1:0".parse().unwrap(),
                config,
            ))
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
            .with_tx(|c| {
                conde::ops::hosts::insert_key(
                    c,
                    user,
                    host,
                    &digest,
                    "test",
                    "2026-10-09T00:00:00.000Z",
                )
            })
            .unwrap();
        key
    }

    /// `conde enroll` with a fresh key: `boards/shared.json`, pinned to the account.
    fn enroll(&self, h: &Host) {
        let key = self.key(&h.user, &h.host);
        let out = h.conde(
            None,
            &[],
            &[
                "enroll",
                "--server",
                &self.url(),
                "--host",
                &h.host,
                "--key-stdin",
            ],
            Some(&key),
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains(&key),
            "the key is never echoed"
        );
        assert!(os::owner_only(
            &h.root
                .join("config")
                .join("conde")
                .join("boards")
                .join("shared.json")
        ));
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
            let d = h.conde(
                None,
                &[("CONDE_SOCK", sock.to_str().unwrap())],
                &["doctor"],
                None,
            );
            String::from_utf8_lossy(&d.stdout)
                .contains("ok      shared: connected")
                .then_some(())
        });
        pid
    }

    /// Publish `name` on the shared board (the human).
    fn publish(&self, h: &Host, sock: &std::path::Path, name: &str) {
        let r = human_call(
            h,
            sock,
            "mailbox.publish",
            json!({"agent": name, "name": name}),
        );
        assert_eq!(
            (r["state"].as_str(), r["name"].as_str()),
            (Some("published"), Some(name)),
            "{r}"
        );
    }

    /// The kind and host `fleet.list` shows for `owner/name` on the board.
    fn fleet_kind(&self, owner: &str, host: &str, name: &str) -> (String, String) {
        let p = self.d.principal(owner, host, false).unwrap();
        let req = conde::protocol::Request::new(conde::util::new_ulid(), "fleet.list", json!({}));
        let resp = self.d.dispatch(&p, &req);
        assert!(resp.ok, "fleet.list: {:?}", resp.error);
        let fleet = resp.result.unwrap();
        let row = fleet["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == format!("{owner}/{name}"))
            .unwrap_or_else(|| panic!("{owner}/{name} not in {fleet}"))
            .clone();
        (
            row["kind"].as_str().unwrap_or_default().into(),
            row["host"].as_str().unwrap_or_default().into(),
        )
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
        let creds = conde::credentials::Credentials {
            server: self.url(),
            host_id: h.host.clone(),
            key,
        };
        conde::credentials::store(&path, &creds).unwrap();
        assert!(os::owner_only(&path));
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
fn human_call(
    h: &Host,
    sock: &std::path::Path,
    op: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let mut req = conde::protocol::Request::new(conde::util::new_ulid(), op, body);
    req.actor = Some(format!("human:{}", h.user));
    req.idempotency_key = Some(conde::util::new_ulid());
    req.board = Some("shared".into());
    let resp = conde::client::Client::connect(sock)
        .unwrap()
        .call(&req)
        .unwrap();
    assert!(resp.ok, "{op}: {:?}", resp.error);
    resp.result.unwrap()
}

fn test_config() -> ServerConfig {
    ServerConfig {
        poll_interval: Duration::from_millis(100),
        ..ServerConfig::default()
    }
}

fn queued_id(out: &str) -> String {
    out.strip_prefix("queued ")
        .and_then(|s| s.split(' ').next())
        .expect(out)
        .to_string()
}

/// The delivery state the sender's `conde sent` shows for `id`, once it is `state`.
fn wait_sent(h: &Host, harness: &str, env: &[(&str, &str)], id: &str, state: &str) -> String {
    wait_for(&format!("delivery {state} of {id}"), || {
        let out = h.ok(Some(harness), env, &["sent", "--board", "shared"]);
        out.lines()
            .find(|l| l.contains(id) && l.contains(&format!("delivery: {state}")))
            .map(str::to_string)
    })
}

#[cfg(unix)]
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
    assert!(
        out.starts_with("registered ann/alice on the local board (codex:t-alice@host-a)"),
        "{out}"
    );
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    let out = b.conde(Some("claude"), CAROL, &["register", "ann/alice"], None);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not in your namespace"),
        "{out:?}"
    );
    let out = a.conde(
        Some("codex"),
        ALICE,
        &["send", "--board", "shared", "ben/carol", "too early"],
        None,
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("alice is not published on shared"),
        "{out:?}"
    );
    cloud.publish(&a, &sa, "alice");
    cloud.publish(&b, &sb, "carol");
    // after enrollment and publication, the server binds mailboxes only, never a session
    let bound = cloud.d.with_conn(|tx| tx.agents_on_host("host-b")).unwrap();
    assert!(!format!("{bound:?}").contains("s-carol"), "{bound:?}");

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(
        Some("codex"),
        ALICE,
        &[
            "send",
            "--board",
            "shared",
            "ben/carol",
            "hello across hosts",
        ],
    ));
    assert!(id.starts_with("shared:"), "{id}");
    let f = carol.wait_frames(&id, 1);
    assert!(
        f[0].text.contains(&format!(
            "\nconde [shared]: new message {id} from ann/alice. Read it with: conde read {id}\n"
        )),
        "{}",
        f[0].text
    );
    wait_sent(&a, "codex", ALICE, &id, "delivered");
    let read = b.ok(Some("claude"), CAROL, &["read", &id]);
    assert!(
        read.contains("From: ann/alice\nTo: ben/carol\n")
            && read.ends_with("\nhello across hosts\n"),
        "{read}"
    );

    // the reply inherits the shared board from the id
    let reply = queued_id(&b.ok(
        Some("claude"),
        CAROL,
        &["send", "ann/alice", "back", "--reply-to", &id],
    ));
    assert!(reply.starts_with("shared:"), "{reply}");

    // doctor on a v2 host: the local board, the shared connection, the delivery slots
    let doctor = String::from_utf8_lossy(&a.conde(Some("codex"), ALICE, &["doctor"], None).stdout)
        .to_string();
    assert!(
        doctor.contains("ok      shared: connected to ws://"),
        "{doctor}"
    );
    assert!(
        doctor.contains("credentials: ") && doctor.contains("boards/shared.json mode 600"),
        "{doctor}"
    );
    assert!(doctor.contains("ok      database: ok"), "{doctor}");
    assert!(doctor.contains("ok      slots: shared "), "{doctor}");

    // a revoked key stops the board connection only; the local board keeps working
    let revoked = cloud
        .d
        .with_tx(|c| conde::ops::hosts::revoke(c, Some("host-a"), None, "now"))
        .unwrap();
    assert_eq!(revoked.len(), 1);
    wait_for("the connection stopped", || {
        let d = String::from_utf8_lossy(&a.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: the board rejected this host's credential")
            .then_some(())
    });
    a.ok(Some("codex"), ALICE, &["send", "alice", "still local"]);
}

#[cfg(unix)]
#[test]
fn a_disabled_users_bridge_retries_and_reconnects_once_enabled() {
    use conde_remote::web::WebStore as _;
    let cloud = Cloud::start(test_config());
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
    let doctor = |h: &Host| {
        String::from_utf8_lossy(&h.conde(None, &[], &["doctor"], None).stdout).to_string()
    };

    // the admin's disable closes the bridge; the daemon keeps its key and retries
    let web = conde::web_store::WebBoard::new(cloud.d.clone());
    assert_eq!(
        web.set_disabled("ben", true).unwrap(),
        Some(vec!["host-b".to_string()])
    );
    let d = wait_for("the connection refused", || {
        let d = doctor(&b);
        (d.contains("shared: stopped") || d.contains("shared: the board's admin disabled"))
            .then_some(d)
    });
    assert!(
        d.contains("shared: the board's admin disabled this user; retrying"),
        "{d}\n{}",
        b.log("daemon.log")
    );

    // enabled again: the same daemon reconnects on its next retry and gets messages
    web.set_disabled("ben", false).unwrap();
    wait_for("the shared board reconnected", || {
        doctor(&b)
            .contains("ok      shared: connected")
            .then_some(())
    });
    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let id = queued_id(&a.ok(
        Some("codex"),
        ALICE,
        &["send", "--board", "shared", "ben/carol", "after the enable"],
    ));
    carol.wait_frames(&id, 1);
    wait_sent(&a, "codex", ALICE, &id, "delivered");

    // a revoked key still stops the connection for good
    cloud
        .d
        .with_tx(|c| conde::ops::hosts::revoke(c, Some("host-b"), None, "now"))
        .unwrap();
    wait_for("the connection stopped", || {
        doctor(&b)
            .contains("shared: stopped: the board rejected this host's credential")
            .then_some(())
    });
}

#[test]
fn a_key_revoked_while_its_user_is_disabled_stops_the_bridge() {
    use conde_remote::web::WebStore as _;
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    let doctor = |h: &Host| {
        String::from_utf8_lossy(&h.conde(None, &[], &["doctor"], None).stdout).to_string()
    };
    let web = conde::web_store::WebBoard::new(cloud.d.clone());
    web.set_disabled("ann", true).unwrap();
    wait_for("the disabled retry", || {
        doctor(&a)
            .contains("shared: the board's admin disabled this user; retrying")
            .then_some(())
    });
    cloud
        .d
        .with_tx(|c| conde::ops::hosts::revoke(c, Some("host-a"), None, "now"))
        .unwrap();
    let d = wait_for("the connection stopped", || {
        let d = doctor(&a);
        d.contains("shared: stopped").then_some(d)
    });
    assert!(
        d.contains("shared: stopped: the board rejected this host's credential"),
        "{d}"
    );
    // the stop is final: an enable does not bring the revoked key back
    web.set_disabled("ann", false).unwrap();
    std::thread::sleep(ms(1500));
    assert!(
        doctor(&a).contains("shared: stopped: the board rejected this host's credential"),
        "{}",
        a.log("daemon.log")
    );
}

#[cfg(unix)]
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
    carol.plan(vec![
        vec![(ms(0), "held", json!({}))],
        vec![(ms(0), "delivered", json!({}))],
    ]);
    let id = queued_id(&a.ok(
        Some("codex"),
        ALICE,
        &[
            "send",
            "--board",
            "shared",
            "ben/carol",
            "survive a restart",
        ],
    ));
    wait_sent(&a, "codex", ALICE, &id, "held");

    // the process ends; the next one is a new bridge instance and the hold is re-attempted
    b.terminate(old);
    b.daemon("new.log", &sb, &[]);
    let f = carol.wait_frames(&id, 2);
    assert_ne!(
        f[0].msg_id, f[1].msg_id,
        "a new token, never the board's attempt id"
    );
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
    let other_sock = a.sock_named("other");
    a.daemon("other.log", &other_sock, &[]);
    wait_for("the first connection fenced", || {
        let d = String::from_utf8_lossy(&a.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: fenced by a newer bridge")
            .then_some(())
    });
    // the first daemon still serves its local board
    a.ok(
        Some("codex"),
        ALICE,
        &["send", "alice", "local after the fence"],
    );
    assert!(
        a.ok(Some("codex"), ALICE, &["inbox", "--board", "local"])
            .contains("local after the fence")
    );
}

#[test]
fn an_idle_connection_stays_up_through_pings() {
    let ping = Duration::from_millis(300);
    let cloud = Cloud::start(ServerConfig {
        ping_interval: ping,
        max_missed_pongs: 2,
        ..test_config()
    });
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    // The board connection drops after 1 s of silence; only pings arrive.
    cloud.join(&mut a, "daemon.log", &sa, &["--idle-timeout", "1"]);
    // A second daemon on the same socket waits for it, then refuses; it never says hello.
    let file = std::fs::File::create(a.root.join("second.log")).unwrap();
    let mut second = a
        .env(&mut std::process::Command::new(BIN))
        .args(["daemon"])
        .stderr(file)
        .spawn()
        .unwrap();
    std::thread::sleep(ping * 7);
    let doctor = a.ok(None, &[], &["doctor"]);
    assert!(
        doctor.contains("1 connection(s) since start"),
        "{doctor}\n{}",
        a.log("daemon.log")
    );
    assert!(
        !a.log("daemon.log").contains("connection lost"),
        "{}",
        a.log("daemon.log")
    );
    assert!(!second.wait().unwrap().success());
    assert!(
        a.log("second.log").contains("another conde daemon"),
        "{}",
        a.log("second.log")
    );
}

#[test]
fn admin_is_human_only_and_healthz_answers() {
    let cloud = Cloud::start(test_config());
    let a = Host::new("ann", "host-a");
    // an agent's shell cannot mint keys
    let out = a.conde(
        Some("codex"),
        ALICE,
        &["admin", "key", "list", "--data", "/nonexistent"],
        None,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("human only"));
    // healthz answers from the store
    let body = cloud.rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(cloud.addr).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).await.unwrap();
        raw
    });
    assert!(body.starts_with("HTTP/1.1 200"), "{body}");
}

#[cfg(unix)]
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
    let id = queued_id(&a.ok(
        Some("codex"),
        ALICE,
        &[
            "send",
            "--board",
            "shared",
            "ben/carol",
            "held, then the session ends",
        ],
    ));
    wait_sent(&a, "codex", ALICE, &id, "held");

    // The session ends: the hold is abandoned (recipient offline, no session detail on the
    // wire) and the re-attempt finds no session.
    let session_file = b
        .root
        .join(format!("claude/sessions/{}.json", std::process::id()));
    let saved = std::fs::read(&session_file).unwrap();
    std::fs::remove_file(&session_file).unwrap();
    wait_sent(
        &a,
        "codex",
        ALICE,
        &id,
        "retrying after failed (recipient offline)",
    );
    let attempts = cloud.attempts(id.trim_start_matches("shared:"));
    assert_eq!(
        (attempts[0].1.as_str(), attempts[0].2.as_deref()),
        ("abandoned", Some("recipient offline"))
    );
    // It comes back: the retry reaches it.
    std::fs::write(&session_file, saved).unwrap();
    carol.wait_frames(&id, 2);
    wait_sent(&a, "codex", ALICE, &id, "notified");
}

/// A host that has not migrated keeps its v1 bridge (`--v1-bridge`, its names on the server)
/// and exchanges messages with a migrated host's published agent, both ways.
#[cfg(unix)]
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
    let out = a.ok(
        Some("claude"),
        &[("CLAUDE_CODE_SESSION_ID", "s-dave")],
        &["register", "dave"],
    );
    assert!(out.contains("ann/dave"), "{out}");
    b.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&b, &sb, "carol");

    carol.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let to_v2 = queued_id(&a.ok(
        Some("claude"),
        &[("CLAUDE_CODE_SESSION_ID", "s-dave")],
        &["send", "ben/carol", "from v1"],
    ));
    let f = carol.wait_frames(&format!("shared:{to_v2}"), 1);
    assert!(f[0].text.contains("from ann/dave"), "{}", f[0].text);

    dave.plan(vec![vec![(ms(0), "delivered", json!({}))]]);
    let to_v1 = queued_id(&b.ok(
        Some("claude"),
        CAROL,
        &["send", "--board", "shared", "ann/dave", "from v2"],
    ));
    let f = dave.wait_frames(to_v1.trim_start_matches("shared:"), 1);
    assert!(
        f[0].text.contains("\nconde: new message "),
        "the v1 bridge injects the board's text: {}",
        f[0].text
    );

    // the v1 bridge's doctor answer carries the build it runs, as the v2 daemon's does
    let req = conde::protocol::Request::new(conde::util::new_ulid(), "doctor", json!({}));
    let doctor = conde::client::Client::connect(&sa)
        .unwrap()
        .call(&req)
        .unwrap();
    assert_eq!(
        doctor.result.as_ref().map(|r| &r["build"]),
        Some(&json!(conde::VERSION)),
        "{doctor:?}"
    );
}

/// Two server replicas on one Postgres board (CONDE_TEST_PG): a message sent
/// through one reaches a host connected to the other by LISTEN/NOTIFY (the
/// poll is far too slow to explain it), and an instance fenced through one
/// replica is closed by the other.
#[cfg(unix)]
#[test]
fn replicas_wake_and_fence_each_other() {
    let Some(db) = pg::fresh_database() else {
        pg::skip("the two-replica test");
        return;
    };
    let slow = ServerConfig {
        poll_interval: Duration::from_secs(30),
        ..ServerConfig::default()
    };
    let (one, two) = (
        Cloud::replica(Some(&db), slow.clone()),
        Cloud::replica(Some(&db), slow),
    );
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
    let id = queued_id(&a.ok(
        Some("codex"),
        ALICE,
        &["send", "--board", "shared", "ben/carol", "across replicas"],
    ));
    carol.wait_frames(&id, 1);
    assert!(
        sent.elapsed() < Duration::from_secs(10),
        "woken, not polled: {:?}",
        sent.elapsed()
    );
    wait_sent(&a, "codex", ALICE, &id, "delivered");

    // a second daemon of host-b on replica two; replica one closes the first instance
    let other_sock = b.sock_named("other");
    let mut cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(b.root.join("config/conde/boards/shared.json")).unwrap(),
    )
    .unwrap();
    cfg["server"] = json!(two.url());
    std::fs::write(
        b.root.join("config/conde/boards/shared.json"),
        cfg.to_string(),
    )
    .unwrap();
    b.daemon("other.log", &other_sock, &[]);
    wait_for("the first instance fenced", || {
        let d = String::from_utf8_lossy(&b.conde(None, &[], &["doctor"], None).stdout).to_string();
        d.contains("shared: stopped: fenced by a newer bridge")
            .then_some(())
    });
}

/// `conde server` on Postgres (CONDE_TEST_PG): ready on /healthz, and SIGTERM
/// closes it cleanly, the board's last handle dropped without a panic.
#[cfg(unix)]
#[test]
fn server_on_postgres_stops_cleanly_on_sigterm() {
    let Some(db) = pg::fresh_database() else {
        pg::skip("conde server on Postgres");
        return;
    };
    // port 0: the server binds and logs its address, so no other process can take it between
    let mut child = std::process::Command::new(BIN)
        .args(["server", "--listen", "127.0.0.1:0"])
        .env("DATABASE_URL", &db)
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut log = String::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(addr) = line
                .split_once("conde server ready addr=")
                .map(|(_, a)| a.trim().to_string())
            {
                let _ = ready_tx.send(addr);
            }
            log.push_str(&line);
            log.push('\n');
        }
        log
    });
    let Ok(addr) = ready_rx.recv_timeout(WAIT) else {
        let _ = child.kill();
        let out = child.wait_with_output().unwrap();
        panic!(
            "no ready line\n{}{}",
            reader.join().unwrap(),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    wait_for("healthz 200", || {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(&addr).ok()?;
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
            .ok()?;
        let mut raw = String::new();
        s.read_to_string(&mut raw).ok()?;
        raw.starts_with("HTTP/1.1 200").then_some(())
    });
    // SAFETY: kill(2) on the child this test spawned.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let out = child.wait_with_output().unwrap();
    let log = format!(
        "{}{}",
        reader.join().unwrap(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "exit {:?}\n{log}", out.status);
    assert!(log.contains("SIGTERM: shutting down"), "{log}");
    assert!(!log.contains("panicked"), "{log}");
}

/// `conde enroll` against the real server: `board.info` over `/v1/rpc` with the host key
/// names the alias (the server's advertised name) and the pinned account; nothing is bound.
#[test]
fn enroll_takes_the_alias_and_account_from_board_info() {
    let cloud = Cloud::start(test_config());
    let h = Host::new("ann", "host-a");
    let key = cloud.key("ann", "host-a");
    let out = h.conde(
        None,
        &[],
        &["enroll", "--server", &cloud.url(), "--key-stdin"],
        Some(&key),
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.starts_with(&format!(
            "enrolled host host-a with {} as board shared (account ann)",
            cloud.url()
        )),
        "{stdout}"
    );
    assert!(!stdout.contains(&key), "the key is never echoed");
    let path = h.root.join("config/conde/boards/shared.json");
    assert!(os::owner_only(&path));
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        (stored["owner"].as_str(), stored["host_id"].as_str()),
        (Some("ann"), Some("host-a"))
    );
    let bound = cloud.d.with_conn(|tx| tx.agents_on_host("host-a")).unwrap();
    assert!(
        bound.is_empty(),
        "enrollment binds nothing on the server: {bound:?}"
    );
}

/// B24: the daemon forwards a human's watch of a shared board with no published name of the
/// human's own (the server scopes it to their agents); an agent session that is not published
/// is still refused before anything leaves the host.
#[cfg(unix)]
#[test]
fn a_human_watches_a_shared_board_without_a_published_name() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    cloud.publish(&a, &sa, "alice");
    a.ok(
        Some("codex"),
        ALICE,
        &["create", "#ops", "--board", "shared"],
    );
    a.ok(
        Some("codex"),
        ALICE,
        &[
            "send",
            "#ops",
            "alice posts on the shared board",
            "--board",
            "shared",
        ],
    );
    let human = a.conde(None, &[], &["register", "ann"], None);
    if String::from_utf8_lossy(&human.stderr).contains("refusing to act as the human") {
        eprintln!("skipped: runs as the human; launch the suite outside an agent shell");
        return;
    }
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );

    // the human: never published, reads alice's post (watch runs until the timeout ends it)
    let out = a
        .env(&mut std::process::Command::new("/usr/bin/timeout"))
        .args(["4", BIN, "watch", "--board", "shared"])
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("ann/alice") && stdout.contains("alice posts on the"),
        "{stdout}{stderr}"
    );

    // an agent session that is not published: refused at once, as before
    let bob: &[(&str, &str)] = &[("CODEX_THREAD_ID", "t-bob")];
    a.ok(Some("codex"), bob, &["register", "bob"]);
    let out = a
        .env(&mut std::process::Command::new(a.root.join("bin/codex")))
        .envs(bob.iter().copied())
        .args([
            "-c",
            "\"$0\" \"$@\"; rc=$?; :; exit $rc",
            "/usr/bin/timeout",
            "4",
            BIN,
            "watch",
            "--board",
            "shared",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "refused, not watching until the timeout: {stderr}"
    );
    assert!(
        stderr.contains("bob is not published on shared"),
        "{stderr}"
    );
}

/// B26: a human with no published name retires an unpublished name on a shared board through
/// the daemon; the board's directory no longer lists it.
#[test]
fn a_human_retires_a_name_on_a_shared_board() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("codex"), ALICE, &["register", "alice"]);
    cloud.publish(&a, &sa, "alice");
    let human = a.conde(None, &[], &["register", "ann"], None);
    if String::from_utf8_lossy(&human.stderr).contains("refusing to act as the human") {
        eprintln!("skipped: runs as the human; launch the suite outside an agent shell");
        return;
    }
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    a.ok(None, &[], &["unpublish", "alice", "--board", "shared"]);
    let out = a.ok(None, &[], &["retire", "alice", "--board", "shared"]);
    assert!(out.contains("retired ann/alice"), "{out}");
    let dir = a.ok(None, &[], &["agents", "--board", "shared"]);
    assert!(!dir.contains("ann/alice"), "{dir}");
}

/// A published name shows its harness kind on the shared board, not `mailbox`; a takeover by
/// another host's session carries the new holder's kind.
#[cfg(unix)]
#[test]
fn a_published_name_keeps_its_harness_kind_on_the_shared_board() {
    let cloud = Cloud::start(test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&a, &sa, "carol");
    assert_eq!(
        cloud.fleet_kind("ann", "host-a", "carol"),
        ("claude".into(), "host-a".into())
    );
    let dir = a.ok(Some("claude"), CAROL, &["agents", "--board", "shared"]);
    assert!(dir.contains("ann/carol\tclaude on host-a"), "{dir}");

    let mut b = Host::new("ann", "host-b");
    cloud.enroll(&b);
    let sb = b.sock();
    cloud.join(&mut b, "daemon.log", &sb, &[]);
    b.ok(Some("codex"), ALICE, &["register", "carol"]);
    let r = human_call(
        &b,
        &sb,
        "mailbox.publish",
        json!({"agent": "carol", "name": "carol", "take": true}),
    );
    assert_eq!(r["state"], "published", "{r}");
    assert_eq!(
        cloud.fleet_kind("ann", "host-b", "carol"),
        ("codex".into(), "host-b".into())
    );
}

/// A name published before the board kept kinds (NULL on the board, nothing acknowledged
/// locally): publishing it again answers `unchanged` and the board then shows its kind.
#[cfg(unix)]
#[test]
fn a_republish_of_an_existing_publication_syncs_the_kind() {
    let database = pg::fresh_database();
    let cloud = Cloud::replica(database.as_deref(), test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&a, &sa, "carol");
    forget_kinds(&cloud, database.as_deref(), &a);
    assert_eq!(cloud.fleet_kind("ann", "host-a", "carol").0, "mailbox");

    let r = human_call(
        &a,
        &sa,
        "mailbox.publish",
        json!({"agent": "carol", "name": "carol"}),
    );
    assert_eq!(
        (r["state"].as_str(), r["unchanged"].as_bool()),
        (Some("published"), Some(true)),
        "{r}"
    );
    assert_eq!(cloud.fleet_kind("ann", "host-a", "carol").0, "claude");
    let acknowledged: Option<String> = rusqlite::Connection::open(a.root.join("data/conde.db"))
        .unwrap()
        .query_row("SELECT kind FROM publications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(acknowledged.as_deref(), Some("claude"));
}

/// What a publication made before kinds looks like: no kind on the board, none acknowledged in
/// `h`'s local board.
fn forget_kinds(cloud: &Cloud, database: Option<&str>, h: &Host) {
    let forget = "UPDATE agents SET actor_kind = NULL WHERE name_lower = 'carol'";
    match database {
        Some(url) => {
            postgres::Client::connect(url, postgres::NoTls)
                .unwrap()
                .batch_execute(forget)
                .unwrap();
        }
        None => cloud
            .d
            .store()
            .as_any()
            .downcast_ref::<conde::store::sqlite::SqliteStore>()
            .unwrap()
            .with_connection(|c| c.execute_batch(forget))
            .unwrap(),
    }
    rusqlite::Connection::open(h.root.join("data/conde.db"))
        .unwrap()
        .execute_batch("UPDATE publications SET kind = NULL")
        .unwrap();
}

/// A daemon holding a publication made before kinds syncs it when it connects, with no publish.
#[cfg(unix)]
#[test]
fn a_connecting_daemon_syncs_the_kind_of_its_existing_publications() {
    let database = pg::fresh_database();
    let cloud = Cloud::replica(database.as_deref(), test_config());
    let mut a = Host::new("ann", "host-a");
    cloud.enroll(&a);
    let sa = a.sock();
    let pid = cloud.join(&mut a, "daemon.log", &sa, &[]);
    a.ok(Some("claude"), CAROL, &["register", "carol"]);
    cloud.publish(&a, &sa, "carol");
    a.terminate(pid);
    forget_kinds(&cloud, database.as_deref(), &a);
    assert_eq!(cloud.fleet_kind("ann", "host-a", "carol").0, "mailbox");

    a.daemon("daemon2.log", &sa, &[]);
    wait_for("the board shows carol's kind", || {
        (cloud.fleet_kind("ann", "host-a", "carol").0 == "claude").then_some(())
    });
    wait_for("the kind recorded as acknowledged", || {
        let kind: Option<String> = rusqlite::Connection::open(a.root.join("data/conde.db"))
            .unwrap()
            .query_row("SELECT kind FROM publications", [], |r| r.get(0))
            .unwrap();
        (kind.as_deref() == Some("claude")).then_some(())
    });
}

/// A VM's SessionStart hook (`infra/agent-vm/conde-register-hook`, beneath a fake Claude Code with
/// the session's JSON on stdin and first boot's `agent.env`) registers `vm.agent` and publishes it,
/// taking it from the person's old host; the owner's machine then reads `connected` while its
/// bridge heartbeats. A second session start publishes nothing new; an `agent.env` without
/// `BOARD` publishes nothing and the hook still succeeds.
#[cfg(unix)]
#[test]
fn a_vm_hook_publishes_the_agent_name_and_the_machine_is_connected() {
    use conde::dispatch::Principal;
    let cloud = Cloud::start(test_config());
    let mut old = Host::new("x", "vm-agent-old");
    cloud.enroll(&old);
    let so = old.sock();
    cloud.join(&mut old, "daemon.log", &so, &[]);
    old.ok(
        Some("claude"),
        &[("CLAUDE_CODE_SESSION_ID", "s-old")],
        &["register", "vm.agent"],
    );
    cloud.publish(&old, &so, "vm.agent");
    let mut vm = Host::new("x", "vm-agent-x");
    cloud.enroll(&vm);
    let sock = vm.sock();
    cloud.join(&mut vm, "daemon.log", &sock, &[]);
    let user_id = cloud.d.principal("x", "vm-agent-x", false).unwrap().user_id;
    cloud
        .d
        .with_tx(|tx| {
            Ok(tx.machine_request(&conde::store::NewMachine {
                user_id,
                host_id: "vm-agent-x",
                owner_object_id: "oid-x",
                vm_size: "Standard_B2as_v2",
                minted_at: Some("2026-11-06T00:00:00.000Z"),
            })?)
        })
        .unwrap();
    let browser = Principal {
        user_id,
        user: "x".into(),
        host: "web".into(),
        local: false,
        bridge_instance: None,
        bridge_connection: None,
        browser: true,
    };
    let state = || {
        let req =
            conde::protocol::Request::new(conde::util::new_ulid(), "machine.status", json!({}));
        let resp = cloud.d.dispatch(&browser, &req);
        assert!(resp.ok, "machine.status: {:?}", resp.error);
        resp.result.unwrap()["state"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        state(),
        "up",
        "the bridge is up, vm.agent is on the old host"
    );

    let env = vm.root.join(".config/agentvm");
    std::fs::create_dir_all(&env).unwrap();
    std::fs::write(
        env.join("agent.env"),
        "AGENT_NAME=vm.agent\nSESSION_ID=s-vm\n",
    )
    .unwrap();
    let bin = vm.root.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(BIN, bin.join("conde")).unwrap();
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/agent-vm/conde-register-hook");
    let session_start = || {
        let mut cmd = os::under(
            &os::harness_file(&vm.root.join("bin"), "claude"),
            "/bin/bash",
        );
        vm.env(&mut cmd)
            .env("PATH", "/usr/bin:/bin")
            .arg(&hook)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        std::io::Write::write_all(
            &mut child.stdin.take().unwrap(),
            br#"{"session_id":"s-vm","hook_event_name":"SessionStart"}"#,
        )
        .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "the hook is quiet on stdout");
        std::fs::read_to_string(env.join("hook.log")).unwrap_or_default()
    };
    let log = session_start();
    assert!(
        log.contains("no BOARD in agent.env: not published"),
        "an agent.env from before the hook published: {log}"
    );
    assert_eq!(state(), "up");

    std::fs::write(
        env.join("agent.env"),
        "AGENT_NAME=vm.agent\nSESSION_ID=s-vm\nBOARD=shared\n",
    )
    .unwrap();
    let log = session_start();
    assert!(
        log.contains("published vm.agent on shared as x/vm.agent"),
        "{log}"
    );
    assert_eq!(
        cloud.fleet_kind("x", "vm-agent-x", "vm.agent"),
        ("claude".into(), "vm-agent-x".into())
    );
    assert_eq!(state(), "connected", "{log}");
    let route = || -> (String, i64) {
        rusqlite::Connection::open(vm.root.join("data/conde.db"))
            .unwrap()
            .query_row(
                "SELECT mailbox_id, route_generation FROM publications",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    let first = route();

    let log = session_start();
    assert_eq!(
        log.matches("published vm.agent on shared").count(),
        2,
        "the next start publishes again, unchanged: {log}"
    );
    assert_eq!(route(), first, "no second publication");
    assert_eq!(state(), "connected");
}
