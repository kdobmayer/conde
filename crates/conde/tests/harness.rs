//! The harness notifier against a fake Claude Code session on this host: a published session
//! file and its messaging socket, which answers frames with receipts on the frame's `from`
//! socket as Claude Code does. Delivery, receipt mapping (and its release), liveness and the
//! doctor lines are checked here; the wire format itself is conde-transport's.
//!
//! The doctor reads the codex home from the environment, so `main` points CODEX_HOME at an
//! empty temp dir before any case runs, and the binary runs without the libtest harness
//! (`harness = false`): its cases run one after another on the main thread.

#[cfg(unix)]
mod serial;

#[cfg(unix)]
fn main() {
    let codex_home = tempfile::tempdir().unwrap();
    // SAFETY: no case has started and no other thread exists yet.
    unsafe { std::env::set_var("CODEX_HOME", codex_home.path()) };
    use cases::*;
    serial::run("harness", &[
        ("a_notification_reaches_its_session_and_its_receipts_map_back_to_the_attempt",
         a_notification_reaches_its_session_and_its_receipts_map_back_to_the_attempt),
        ("a_session_that_is_not_published_fails_and_is_not_live", a_session_that_is_not_published_fails_and_is_not_live),
        ("a_session_nobody_listens_on_fails_with_the_socket_named", a_session_nobody_listens_on_fails_with_the_socket_named),
        ("doctor_names_the_callers_session_and_scopes_issues_to_its_harness",
         doctor_names_the_callers_session_and_scopes_issues_to_its_harness),
        ("the_sessions_dir_follows_the_given_config", the_sessions_dir_follows_the_given_config),
    ]);
}

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
mod cases {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use conde::address::{Address, Kind};
    use conde::harness::{HarnessNotifier, claude_sessions_dir, doctor_checks};
    use conde::notify::{Notifier, NotifyOutcome, Receipt, ReceiptStatus};
    use conde_transport::msg_id_for;
    use serde_json::{Value, json};

    const SID: &str = "sid-harness-1";

    /// A Claude config dir with one live session (this process's pid, so discovery trusts it) whose
    /// messaging socket the test serves.
    struct Session {
        dir: tempfile::TempDir,
        listener: UnixListener,
    }

    impl Session {
        fn new() -> Session {
            let dir = tempfile::tempdir().unwrap();
            let sessions = dir.path().join("config/sessions");
            std::fs::create_dir_all(&sessions).unwrap();
            let sock = dir.path().join("s.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let info = json!({"pid": std::process::id(), "sessionId": SID, "name": "worker", "status": "idle",
                              "cwd": "/", "messagingSocketPath": sock});
            std::fs::write(sessions.join("1.json"), info.to_string()).unwrap();
            Session { dir, listener }
        }

        fn config(&self) -> PathBuf {
            self.dir.path().join("config")
        }

        /// Serves `frames` frames; for each, sends back the receipts `respond` names
        /// (status, extra fields), and passes the frame to the test.
        fn serve(self, frames: usize, respond: fn(&Value) -> Vec<(&'static str, Value)>) -> (mpsc::Receiver<Value>, tempfile::TempDir) {
            let (tx, rx) = mpsc::channel();
            let Session { dir, listener } = self;
            thread::spawn(move || {
                for _ in 0..frames {
                    let (conn, _) = listener.accept().unwrap();
                    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let last = BufReader::new(conn).lines().map_while(Result::ok).last().unwrap();
                    let frame: Value = serde_json::from_str(&last).unwrap();
                    for (status, extra) in respond(&frame) {
                        let mut receipt = json!({"type": "control", "action": "peer_message_status", "status": status,
                                                 "orig_msg_id": frame["msg_id"]});
                        for (k, v) in extra.as_object().unwrap() {
                            receipt[k] = v.clone();
                        }
                        let mut c = UnixStream::connect(uds_path(frame["from"].as_str().unwrap())).unwrap();
                        c.write_all(format!("{receipt}\n").as_bytes()).unwrap();
                    }
                    tx.send(frame).unwrap();
                }
            });
            (rx, dir)
        }
    }

    /// The path a `uds:<percent-encoded path>` reply address names.
    fn uds_path(addr: &str) -> PathBuf {
        let raw = addr.strip_prefix("uds:").unwrap().as_bytes();
        let (mut out, mut i) = (Vec::new(), 0);
        while i < raw.len() {
            if raw[i] == b'%' {
                out.push(u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).unwrap(), 16).unwrap());
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        PathBuf::from(std::ffi::OsStr::from_bytes(&out))
    }

    fn claude(target: &str) -> Address {
        Address::new(Kind::Claude, target, "box")
    }

    /// Polls until `want` receipts arrived or `within` passed.
    fn receipts(n: &mut HarnessNotifier, want: usize, within: Duration) -> Vec<Receipt> {
        let deadline = Instant::now() + within;
        let mut got = Vec::new();
        while got.len() < want && Instant::now() < deadline {
            got.extend(n.poll_receipts());
        }
        got
    }

    pub fn a_notification_reaches_its_session_and_its_receipts_map_back_to_the_attempt() {
        let s = Session::new();
        let config = s.config();
        let (frames, _dir) = s.serve(2, |frame| {
            if frame["message"]["content"].as_str().unwrap().contains("first") {
                // a hold then the delivery; a status the transport does not know; a receipt for
                // a frame this notifier never sent
                vec![
                    ("held", json!({})),
                    ("delivered", json!({})),
                    ("pondering", json!({})),
                    ("delivered", json!({"orig_msg_id": "not-ours"})),
                ]
            } else {
                vec![("dropped", json!({"drop_reason": "queue full"}))]
            }
        });
        let mut n = HarnessNotifier::new(Some(config));

        assert_eq!(n.notify(&claude(SID), "the first note", "att-1"), NotifyOutcome::Accepted);
        let frame = frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let reply = uds_path(frame["from"].as_str().unwrap());
        assert_eq!(frame["msg_id"].as_str(), Some(msg_id_for("att-1").as_str()), "the frame id is derived from the attempt");
        assert_eq!(frame["session_id"], SID);
        assert!(frame["message"]["content"].as_str().unwrap().contains("the first note"), "{frame}");
        let got = receipts(&mut n, 2, Duration::from_secs(5));
        assert_eq!(
            got,
            [
                Receipt { attempt_id: "att-1".into(), status: ReceiptStatus::Held },
                Receipt { attempt_id: "att-1".into(), status: ReceiptStatus::Delivered },
            ],
            "only the two known receipts for our frame"
        );
        assert_eq!(n.mappings(), 1);

        // A drop carries its reason; one reply socket serves every frame to this session's dir.
        assert_eq!(n.notify(&claude(SID), "the second note", "att-2"), NotifyOutcome::Accepted);
        frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let got = receipts(&mut n, 1, Duration::from_secs(5));
        assert_eq!(got, [Receipt { attempt_id: "att-2".into(), status: ReceiptStatus::Dropped { reason: "queue full".into() } }]);

        // Once forgotten, an attempt's late receipts are no longer reported: a late receipt for the
        // forgotten att-1 is read and dropped, while att-2's after it, on the same connection, maps.
        n.forget("att-1");
        assert_eq!(n.mappings(), 1);
        let mut late = UnixStream::connect(&reply).unwrap();
        for attempt in ["att-1", "att-2"] {
            let receipt = json!({"type": "control", "action": "peer_message_status", "status": "delivered",
                                 "orig_msg_id": msg_id_for(attempt)});
            late.write_all(format!("{receipt}\n").as_bytes()).unwrap();
        }
        drop(late);
        let got = receipts(&mut n, 1, Duration::from_secs(5));
        assert_eq!(got, [Receipt { attempt_id: "att-2".into(), status: ReceiptStatus::Delivered }]);
        assert!(n.poll_receipts().is_empty(), "nothing else arrived");
        n.forget("att-2");
        assert_eq!(n.mappings(), 0);
    }

    pub fn a_session_that_is_not_published_fails_and_is_not_live() {
        let s = Session::new();
        let mut n = HarnessNotifier::new(Some(s.config()));
        match n.notify(&claude("sid-elsewhere"), "hi", "att-x") {
            NotifyOutcome::Failed(why) => assert_eq!(why, "no live Claude session sid-elsewhere"),
            other => panic!("{other:?}"),
        }
        assert_eq!(n.mappings(), 0, "nothing to map without a frame");
        assert!(n.is_live(&claude(SID)));
        assert!(!n.is_live(&claude("sid-elsewhere")));
        assert_eq!(n.live_claude(), [SID]);
        // A Codex thread stays resumable and a human has no incarnation to lose; a mailbox is no harness.
        assert!(n.is_live(&Address::new(Kind::Codex, "thread-1", "box")));
        assert!(n.is_live(&Address::new(Kind::Human, "krisz", "box")));
        assert!(!n.is_live(&Address::new(Kind::Mailbox, "01MB", "box")));
        for (kind, why) in [(Kind::Human, "no push transport for humans"), (Kind::Mailbox, "a mailbox is not a harness")] {
            assert_eq!(n.notify(&Address::new(kind, "t", "box"), "hi", "att-y"), NotifyOutcome::Failed(why.into()));
        }
    }

    pub fn a_session_nobody_listens_on_fails_with_the_socket_named() {
        let s = Session::new();
        let config = s.config();
        let sock = s.dir.path().join("s.sock");
        // The published entry and its socket file stay; nothing accepts on it any more.
        let Session { dir: _dir, listener } = s;
        drop(listener);
        let mut n = HarnessNotifier::new(Some(config));
        match n.notify(&claude(SID), "hi", "att-z") {
            NotifyOutcome::Failed(why) => {
                assert!(why.starts_with(&format!("cannot deliver to {}: ", sock.display())), "{why}")
            }
            other => panic!("{other:?}"),
        }
        // The mapping is made before the send, so a failed send holds one until the attempt is forgotten.
        assert_eq!(n.mappings(), 1);
        n.forget("att-z");
        assert_eq!(n.mappings(), 0);
    }

    fn line<'a>(checks: &'a [(bool, String)], prefix: &str) -> Vec<&'a (bool, String)> {
        checks.iter().filter(|(_, t)| t.starts_with(prefix)).collect()
    }

    pub fn doctor_names_the_callers_session_and_scopes_issues_to_its_harness() {
        let s = Session::new();
        let n = HarnessNotifier::new(Some(s.config()));
        let sessions = s.config().join("sessions");

        let checks = doctor_checks(&n, Some(&format!("claude:{SID}")));
        let claude = line(&checks, "claude:");
        assert_eq!(claude[0], &(true, format!("claude: sessions dir {} readable, 1 live session(s)", sessions.display())));
        assert_eq!(claude[1], &(true, format!("claude: this session {SID} is discovered; notifications can reach it")));
        // CODEX_HOME (set by `main`) has no app-server socket: no issue for a Claude caller, one for
        // a Codex caller
        let codex_sock = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap().join("app-server-control/app-server-control.sock");
        let no_socket = format!(
            "codex: no app-server control socket at {}; codex notifications fall back to `codex queue` (picked up at the next turn)",
            codex_sock.display()
        );
        assert_eq!(line(&checks, "codex:"), [&(true, no_socket.clone())]);
        assert_eq!(line(&doctor_checks(&n, Some("codex:thread-1")), "codex:"), [&(false, no_socket)]);

        let checks = doctor_checks(&n, Some("claude:sid-elsewhere"));
        let (ok, text) = line(&checks, "claude: this session")[0];
        assert!(!ok && text.contains("is not discovered in") && text.contains("notifications cannot reach it"), "{text}");

        // a caller outside Claude gets no per-session line
        assert!(line(&doctor_checks(&n, Some("codex:thread-1")), "claude: this session").is_empty());

        // an unreadable sessions dir is an issue only for a Claude caller
        let gone = HarnessNotifier::new(Some(Path::new("/nonexistent-conde-config").to_path_buf()));
        for (caller, issue) in [("claude:x", true), ("codex:t", false)] {
            let checks = doctor_checks(&gone, Some(caller));
            let (ok, text) = line(&checks, "claude: sessions dir")[0];
            assert!(text.contains("not readable"), "{text}");
            assert_eq!(!ok, issue, "{caller}: {text}");
        }
    }

    pub fn the_sessions_dir_follows_the_given_config() {
        assert_eq!(claude_sessions_dir(Some(Path::new("/x/claude"))), Some(PathBuf::from("/x/claude/sessions")));
    }
}
