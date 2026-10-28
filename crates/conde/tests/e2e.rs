//! The real binary: `conde daemon` on a temp socket and board, driven by the
//! CLI under fake harness parents (a process named `codex` / `claude`).
#![cfg(unix)] // drives the daemon over a unix socket

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_conde");

struct Daemon {
    root: PathBuf,
    child: Option<Child>,
}

impl Daemon {
    fn root() -> PathBuf {
        let root =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("e2e-{}", conde::util::new_ulid()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for harness in ["codex", "claude"] {
            std::os::unix::fs::symlink("/bin/sh", root.join("bin").join(harness)).unwrap();
        }
        root
    }

    fn start() -> Daemon {
        let mut d = Daemon {
            root: Daemon::root(),
            child: None,
        };
        let log = std::fs::File::create(d.root.join("daemon.log")).unwrap();
        let child = d
            .base(&mut Command::new(BIN))
            .args(["daemon", "--interval", "0.1"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        d.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(d.sock()).is_err() {
            assert!(
                Instant::now() < deadline,
                "daemon did not come up: {}",
                d.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    fn sock(&self) -> PathBuf {
        self.root.join("run/conde.sock")
    }

    /// A clean environment: no inherited harness markers, temp board only.
    fn base<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        // An empty PATH: a codex delivery must never start the real codex.
        cmd.env_clear()
            .env("PATH", self.root.join("nopath"))
            .env("HOME", &self.root)
            .env("USER", "tester")
            .env("CONDE_DATA", self.root.join("data"))
            .env("CONDE_SOCK", self.sock())
            .env("CONDE_HOST", "testhost")
    }

    /// Run the CLI, optionally beneath a fake harness process.
    fn conde(&self, harness: Option<&str>, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = match harness {
            Some(h) => {
                let mut c = Command::new(self.root.join("bin").join(h));
                // the trailing ':' keeps the shell from exec-replacing itself
                c.args(["-c", "\"$0\" \"$@\"; rc=$?; :; exit $rc", BIN]);
                c
            }
            None => Command::new(BIN),
        };
        self.base(&mut cmd)
            .envs(env.iter().copied())
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, harness: Option<&str>, env: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.conde(harness, env, args);
        assert!(
            out.status.success(),
            "conde {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }

    fn wait_log(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.log().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "daemon log never showed {needle:?}:\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const ALICE: &[(&str, &str)] = &[
    ("CODEX_THREAD_ID", "t-alice"),
    ("CLAUDE_CODE_SESSION_ID", "inherited-noise"),
];
const BOB: &[(&str, &str)] = &[("CODEX_THREAD_ID", "t-bob")];

#[test]
fn dm_and_channel_mention_end_to_end() {
    let d = Daemon::start();
    let codex = Some("codex");
    assert_eq!(
        d.ok(codex, ALICE, &["register", "alice"]),
        "registered tester/alice on the local board (codex:t-alice@testhost)\n"
    );
    assert_eq!(
        d.ok(codex, BOB, &["register", "Bob"]),
        "registered tester/bob on the local board (codex:t-bob@testhost)\n"
    );
    assert_eq!(
        d.ok(codex, ALICE, &["whoami"]),
        "tester/alice (local, codex:t-alice@testhost)\n"
    );

    // DM: send, push, inbox, read, receipt
    let out = d.ok(codex, ALICE, &["send", "bob", "hello bob\nline two"]);
    let id = out
        .strip_prefix("queued ")
        .and_then(|s| s.strip_suffix(" to tester/bob\n"))
        .expect(&out)
        .to_string();
    d.wait_log(&format!(
        "conde: new message {id} from tester/alice. Read it with: conde read {id}"
    ));
    assert!(d.log().contains("codex:t-bob@testhost"));
    let inbox = d.ok(codex, BOB, &["inbox"]);
    assert!(
        inbox.starts_with(&format!("local\tdm\t{id}\ttester/alice\t20")),
        "{inbox}"
    );
    assert!(inbox.ends_with("\thello bob\n"), "{inbox}");
    let read = d.ok(codex, BOB, &["read", &id]);
    assert!(
        read.starts_with(&format!("=== {id}\nFrom: tester/alice\nTo: tester/bob\n")),
        "{read}"
    );
    assert!(read.ends_with("\nhello bob\nline two\n"), "{read}");
    let sent = d.ok(codex, ALICE, &["sent"]);
    assert!(
        sent.starts_with(&format!("local  {id}  tester/bob  read 20")),
        "{sent}"
    );

    // channel with a mention
    assert_eq!(
        d.ok(codex, ALICE, &["create", "#dev"]),
        "created #dev (you are a member)\n"
    );
    assert!(
        d.ok(codex, ALICE, &["invite", "bob", "#dev"])
            .starts_with("invited tester/bob to #dev")
    );
    let out = d.ok(codex, BOB, &["send", "#dev", "@alice please review"]);
    let post = out
        .strip_prefix("queued ")
        .and_then(|s| s.strip_suffix(" to #dev\n"))
        .expect(&out)
        .to_string();
    d.wait_log(&format!(
        "conde: you were mentioned in #dev by tester/bob (message {post})"
    ));
    let inbox = d.ok(codex, ALICE, &["inbox"]);
    assert!(
        inbox.starts_with(&format!("local\t#dev\t{post}\ttester/bob\t")),
        "{inbox}"
    );
    let read = d.ok(codex, ALICE, &["read", "#dev"]);
    assert!(read.starts_with("--- #dev\n"), "{read}");
    assert!(read.contains("@alice please review"), "{read}");
    assert_eq!(
        d.ok(codex, ALICE, &["members", "#dev"]),
        "tester/alice\tup to date\ntester/bob\t2 behind\n"
    );

    // reactions toggle, search
    assert_eq!(
        d.ok(codex, ALICE, &["react", &post]),
        format!("reacted 👍 to {post} in #dev\n")
    );
    assert!(
        d.ok(codex, ALICE, &["show", &post])
            .contains("Reaction: 👍 tester/alice\n")
    );
    assert_eq!(
        d.ok(codex, ALICE, &["react", &post]),
        format!("removed 👍 from {post} in #dev\n")
    );
    let hits = d.ok(codex, BOB, &["search", "REVIEW"]);
    assert!(
        hits.starts_with(&format!("local\t#dev\t{post}\ttester/bob\t")),
        "{hits}"
    );

    // attachments travel inline and come back byte for byte
    let src = d.root.join("note.txt");
    std::fs::write(&src, "attached bytes").unwrap();
    d.ok(
        codex,
        ALICE,
        &["send", "bob", "see file", "--attach", src.to_str().unwrap()],
    );
    let read = d.ok(codex, BOB, &["read"]);
    let att = read
        .lines()
        .find_map(|l| l.split("conde attachment ").nth(1))
        .expect(&read)
        .trim_end_matches(')')
        .to_string();
    let outdir = d.root.join("out");
    std::fs::create_dir(&outdir).unwrap();
    d.ok(
        codex,
        BOB,
        &["attachment", &att, "--out", outdir.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read_to_string(outdir.join("note.txt")).unwrap(),
        "attached bytes"
    );

    // the board file is the server's: 0700 data dir, 0600 socket
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(d.sock()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(d.root.join("data/conde.db").exists());
}

#[test]
fn identity_follows_the_nearest_harness() {
    let d = Daemon::start();
    let both = &[
        ("CODEX_THREAD_ID", "t-1"),
        ("CLAUDE_CODE_SESSION_ID", "s-1"),
    ];
    // under codex the codex marker wins; under claude the claude marker does
    assert!(
        d.ok(Some("codex"), both, &["register", "vegrehajtas.codex"])
            .ends_with(" on the local board (codex:t-1@testhost)\n")
    );
    assert!(
        d.ok(Some("claude"), both, &["register", "kond.dev.claude"])
            .ends_with(" on the local board (claude:s-1@testhost)\n")
    );
    assert_eq!(
        d.ok(Some("claude"), both, &["whoami"]),
        "tester/kond.dev.claude (local, claude:s-1@testhost)\n"
    );

    // a missing codex registration never falls back to the inherited claude one
    d.ok(Some("codex"), both, &["unregister", "vegrehajtas.codex"]);
    let out = d.conde(Some("codex"), both, &["whoami"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("codex:t-1"), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());

    // a marker-less shell under a harness must not act as the human
    let out = d.conde(Some("codex"), &[], &["register", "krisz"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to act as the human"));

    // nor may a Codex child inherit its parent's Claude identity
    let out = d.conde(
        Some("codex"),
        &[("CLAUDE_CODE_SESSION_ID", "s-1")],
        &["whoami"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("marker is inherited"));

    // self-only registration
    let out = d.conde(
        Some("codex"),
        both,
        &["register", "overseer", "claude:4c064766-0000"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("self-only"));
}

#[test]
fn malformed_and_unknown_frames_get_error_responses() {
    let d = Daemon::start();
    let stream = UnixStream::connect(d.sock()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream;
    let mut ask = |line: &str| -> serde_json::Value {
        w.write_all(line.as_bytes()).unwrap();
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        serde_json::from_str(&buf).unwrap()
    };
    let r = ask("not json\n");
    assert_eq!(
        (r["ok"].as_bool(), r["error"]["code"].as_str()),
        (Some(false), Some("bad_request"))
    );
    let r = ask("{\"v\":1,\"type\":\"request\",\"request_id\":\"r1\",\"op\":\"nope\"}\n");
    assert_eq!(r["request_id"], "r1");
    assert_eq!(r["error"]["code"], "bad_request");
    let r = ask("{\"v\":1,\"type\":\"request\",\"request_id\":\"r2\",\"op\":\"bridge.claim\"}\n");
    assert_eq!(r["error"]["code"], "forbidden");
    let r = ask("{\"v\":1,\"type\":\"request\",\"request_id\":\"r3\",\"op\":\"channel.list\"}\n");
    assert_eq!(r["ok"], true);
    assert_eq!(r["type"], "response");
}

#[test]
fn second_daemon_refuses_a_live_socket_and_once_runs_a_pass() {
    let d = Daemon::start();
    let out = d
        .base(&mut Command::new(BIN))
        .args(["daemon"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("another conde daemon is listening"));

    let fresh = Daemon {
        root: Daemon::root(),
        child: None,
    };
    let out = fresh
        .base(&mut Command::new(BIN))
        .args(["daemon", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fresh.root.join("data/conde.db").exists());
}
