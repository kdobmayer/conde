//! Local mode end to end: `conde daemon` delivers into a fake Claude Code
//! session over its socket, and receipts drive the notification state that
//! `conde sent` shows.
#![cfg(unix)] // fakes Claude Code's unix-socket inbox (tests/support FakeClaude)

mod support;

use serde_json::json;
use support::*;

const ALICE: &[(&str, &str)] = &[("CODEX_THREAD_ID", "t-alice")];
const CAROL: &[(&str, &str)] = &[("CLAUDE_CODE_SESSION_ID", "s-carol")];

fn queued_id(out: &str) -> String {
    out.strip_prefix("queued ")
        .and_then(|s| s.split(' ').next())
        .expect(out)
        .to_string()
}

#[test]
fn local_daemon_delivers_to_claude_and_follows_receipts() {
    let mut h = Host::new("tester", "testhost");
    let carol = FakeClaude::start(&h, "s-carol");
    let sock = h.sock();
    let pid = h.daemon(
        "daemon.log",
        &sock,
        &["--interval", "0.1", "--retry-backoff", "1.5"],
    );
    h.ok(Some("codex"), ALICE, &["register", "alice"]);
    h.ok(Some("claude"), CAROL, &["register", "carol"]);

    // held, then the recipient's user approves: delivered
    carol.plan(vec![vec![
        (ms(0), "held", json!({})),
        (ms(1500), "delivered", json!({})),
    ]]);
    let held = queued_id(&h.ok(Some("codex"), ALICE, &["send", "carol", "needs approval"]));
    h.wait_delivery("codex", ALICE, &held, "held");
    h.wait_delivery("codex", ALICE, &held, "delivered");
    let f = carol.frames_for(&held);
    assert_eq!(f.len(), 1, "a hold is never resent");
    assert!(
        f[0].text
            .contains(&format!("conde: new message {held} from tester/alice")),
        "{}",
        f[0].text
    );
    assert!(
        f[0].text.starts_with("<cross-session-message from=\"uds:"),
        "{}",
        f[0].text
    );
    assert!(
        !f[0].raw.contains("from-mode"),
        "the sender never asserts a permission mode: {}",
        f[0].raw
    );
    assert_eq!(f[0].msg_id.len(), 32);
    // the reply identity lives next to the recipient's socket, named by the daemon's pid
    assert!(
        f[0].from.ends_with(&format!("/cc/{pid}.sock")),
        "{}",
        f[0].from
    );

    // expired: retried after the backoff with a fresh attempt, then accepted (no receipt)
    carol.plan(vec![vec![(ms(0), "expired", json!({}))], vec![]]);
    let expired = queued_id(&h.ok(Some("codex"), ALICE, &["send", "carol", "expire once"]));
    let line = h.wait_delivery("codex", ALICE, &expired, "retrying after expired");
    assert!(line.contains("  unread  "), "{line}");
    let f = carol.wait_frames(&expired, 2);
    assert_ne!(f[0].msg_id, f[1].msg_id, "one msg_id per attempt");
    h.wait_delivery("codex", ALICE, &expired, "notified");

    // refused (wire form: expired with detail refused): terminal, never resent
    carol.plan(vec![vec![(
        ms(0),
        "expired",
        json!({"status_detail": "refused"}),
    )]]);
    let refused = queued_id(&h.ok(Some("codex"), ALICE, &["send", "carol", "opted out"]));
    h.wait_delivery("codex", ALICE, &refused, "refused");
    std::thread::sleep(ms(2500)); // longer than the retry backoff
    assert_eq!(carol.frames_for(&refused).len(), 1, "refused is terminal");
    assert_eq!(
        carol.frames_for(&expired).len(),
        2,
        "accepted is not resent"
    );
    assert!(
        h.sent_line("codex", ALICE, &refused)
            .ends_with("delivery: refused")
    );

    // the recipient reads; doctor sees this session from the daemon's side
    let read = h.ok(Some("claude"), CAROL, &["read", &held]);
    assert!(read.ends_with("\nneeds approval\n"), "{read}");
    let doctor = h.ok(Some("claude"), CAROL, &["doctor"]);
    assert!(
        doctor.contains("ok      claude: this session s-carol is discovered"),
        "{doctor}"
    );
    assert!(doctor.contains("1 live session(s)"), "{doctor}");

    // SIGTERM: clean exit, the CLI socket and the Claude reply socket are gone
    let status = h.terminate(pid);
    assert!(status.success(), "{status:?}: {}", h.log("daemon.log"));
    assert!(!sock.exists());
    assert!(!h.root.join(format!("cc/{pid}.sock")).exists());
    assert!(h.log("daemon.log").contains("SIGTERM"));
}

#[test]
fn coalesced_drop_settles_every_attempt_it_names() {
    let mut h = Host::new("tester", "testhost");
    let carol = FakeClaude::start(&h, "s-carol");
    let sock = h.sock();
    h.daemon(
        "daemon.log",
        &sock,
        &["--interval", "0.1", "--retry-backoff", "30"],
    );
    h.ok(Some("codex"), ALICE, &["register", "alice"]);
    h.ok(Some("claude"), CAROL, &["register", "carol"]);
    // The first frame is accepted silently; the second answers with one drop
    // frame naming both its own id and the first one's.
    carol.plan(vec![vec![]]);
    let first = queued_id(&h.ok(Some("codex"), ALICE, &["send", "carol", "one"]));
    let first_frame = carol.wait_frames(&first, 1).remove(0);
    h.wait_delivery("codex", ALICE, &first, "notified");
    carol.plan(vec![vec![(
        ms(0),
        "dropped",
        json!({"drop_reason": "queue-full", "dropped_msg_ids": [first_frame.msg_id]}),
    )]]);
    let second = queued_id(&h.ok(Some("codex"), ALICE, &["send", "carol", "two"]));
    h.wait_delivery(
        "codex",
        ALICE,
        &second,
        "retrying after dropped (queue-full)",
    );
    h.wait_delivery(
        "codex",
        ALICE,
        &first,
        "retrying after dropped (queue-full)",
    );
}

#[test]
fn a_codex_child_cannot_unregister_another_sessions_name() {
    let mut h = Host::new("tester", "testhost");
    let sock = h.sock();
    h.daemon("daemon.log", &sock, &["--interval", "0.2"]);
    h.ok(Some("codex"), ALICE, &["register", "alice"]);
    let out = h.conde(
        Some("codex"),
        &[("CODEX_THREAD_ID", "t-other")],
        &["unregister", "alice"],
        None,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("self-only"));
    assert_eq!(
        h.ok(Some("codex"), ALICE, &["whoami"]),
        "tester/alice (local, codex:t-alice@testhost)\n"
    );
}
