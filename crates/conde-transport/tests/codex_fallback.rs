//! Codex delivery when direct injection is not possible: the app-server daemon is started when
//! its control socket is missing, and `codex queue` takes the message when the direct path
//! fails. A fake `codex` on PATH records each call and answers as configured through its
//! environment. One test in its own binary: it changes the process environment (PATH,
//! CODEX_HOME, HOME), which nothing else here reads concurrently.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use conde_transport::{CodexOutcome, PROVENANCE, codex_control_socket, codex_notify_with_id};

/// Records each call's arguments (0x1e between arguments, 0x1d after each call: the message has
/// newlines) and whether CODEX_THREAD_ID reached it; then answers per FAKE_* variables.
const FAKE_CODEX: &str = r#"#!/bin/sh
{ for a in "$@"; do printf '%s\036' "$a"; done; printf 'thread=%s\035' "${CODEX_THREAD_ID:-}"; } >> "$FAKE_LOG"
case "$1" in
app-server)
    [ -n "${FAKE_DAEMON_MAKES:-}" ] && mkdir -p "$(dirname "$FAKE_DAEMON_MAKES")" && : > "$FAKE_DAEMON_MAKES"
    exit "${FAKE_DAEMON_EXIT:-0}" ;;
queue)
    [ -n "${FAKE_QUEUE_STDERR:-}" ] && { echo "$FAKE_QUEUE_STDERR" >&2; exit 1; }
    [ -n "${FAKE_QUEUE_STDOUT:-}" ] && { echo "$FAKE_QUEUE_STDOUT"; exit 1; }
    exit 0 ;;
esac
exit 2
"#;

fn set(name: &str, value: Option<&str>) {
    // SAFETY: this binary's only test; no other thread reads the environment.
    match value {
        Some(v) => unsafe { std::env::set_var(name, v) },
        None => unsafe { std::env::remove_var(name) },
    }
}

/// The calls the fake recorded since the log was last cleared: (args, CODEX_THREAD_ID seen).
fn calls(log: &Path) -> Vec<(Vec<String>, String)> {
    let raw = std::fs::read_to_string(log).unwrap_or_default();
    let _ = std::fs::remove_file(log);
    raw.split('\u{1d}')
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts: Vec<String> = l.split('\u{1e}').map(str::to_string).collect();
            let thread = parts.pop().unwrap().trim_start_matches("thread=").to_string();
            (parts, thread)
        })
        .collect()
}

fn daemon_start() -> Vec<String> {
    ["app-server", "daemon", "start"].map(String::from).to_vec()
}

fn queue(thread: &str, text: &str) -> Vec<String> {
    vec!["queue".into(), "--thread".into(), thread.into(), "--message".into(), format!("{PROVENANCE}{text}")]
}

#[test]
fn codex_delivery_falls_back_to_the_daemon_start_and_codex_queue() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("codex"), FAKE_CODEX).unwrap();
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = dir.path().join("codex-home");
    let sock = codex_control_socket(&home);
    let log = dir.path().join("calls");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    set("PATH", Some(&path));
    set("CODEX_HOME", Some(home.to_str().unwrap()));
    set("FAKE_LOG", Some(log.to_str().unwrap()));
    // the daemon must not inherit the calling session's marker
    set("CODEX_THREAD_ID", Some("parent-thread"));
    for v in ["FAKE_DAEMON_MAKES", "FAKE_DAEMON_EXIT", "FAKE_QUEUE_STDERR", "FAKE_QUEUE_STDOUT"] {
        set(v, None);
    }

    // No control socket and the daemon will not start: `codex queue` takes the message, with the
    // provenance line in front.
    set("FAKE_DAEMON_EXIT", Some("1"));
    assert_eq!(codex_notify_with_id("th-1", "hello", Some("att-1")).unwrap(), CodexOutcome::Queued);
    let seen = calls(&log);
    assert_eq!(seen.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>(), [daemon_start(), queue("th-1", "hello")]);
    assert_eq!(seen[0].1, "", "CODEX_THREAD_ID is scrubbed from the daemon's environment");

    // The queue fails too: both reasons, the queue's stderr verbatim.
    set("FAKE_QUEUE_STDERR", Some("thread th-1 not found"));
    let err = codex_notify_with_id("th-1", "hello", None).unwrap_err().to_string();
    assert_eq!(
        err,
        "codex delivery failed: codex app-server daemon start exited with exit status: 1; \
         codex queue fallback also failed: thread th-1 not found"
    );
    calls(&log);
    // A queue that explains itself on stdout only is quoted from there.
    set("FAKE_QUEUE_STDERR", None);
    set("FAKE_QUEUE_STDOUT", Some("no such thread"));
    let err = codex_notify_with_id("th-1", "hello", None).unwrap_err().to_string();
    assert!(err.ends_with("codex queue fallback also failed: no such thread"), "{err}");
    calls(&log);
    set("FAKE_QUEUE_STDOUT", None);

    // The daemon starts but its socket never appears.
    set("FAKE_DAEMON_EXIT", Some("0"));
    set("FAKE_QUEUE_STDERR", Some("busy"));
    let err = codex_notify_with_id("th-1", "hello", None).unwrap_err().to_string();
    assert!(err.contains(&format!("daemon started but socket missing: {}", sock.display())), "{err}");
    calls(&log);
    set("FAKE_QUEUE_STDERR", None);

    // The daemon creates its socket path, but what is there does not answer: the direct turn
    // fails and the queue still delivers.
    set("FAKE_DAEMON_MAKES", Some(sock.to_str().unwrap()));
    assert_eq!(codex_notify_with_id("th-2", "again", None).unwrap(), CodexOutcome::Queued);
    let seen: Vec<_> = calls(&log).into_iter().map(|(a, _)| a).collect();
    assert_eq!(seen, [daemon_start(), queue("th-2", "again")]);
    // Now present: no second daemon start, straight to the (failing) socket, then the queue.
    assert_eq!(codex_notify_with_id("th-2", "third", None).unwrap(), CodexOutcome::Queued);
    let seen: Vec<_> = calls(&log).into_iter().map(|(a, _)| a).collect();
    assert_eq!(seen, [queue("th-2", "third")]);
    set("FAKE_DAEMON_MAKES", None);
    std::fs::remove_file(&sock).unwrap();

    // Without a codex home at all, the queue is the only path.
    set("CODEX_HOME", None);
    let saved_home = std::env::var("HOME").ok();
    set("HOME", None);
    assert_eq!(codex_notify_with_id("th-3", "homeless", None).unwrap(), CodexOutcome::Queued);
    let seen: Vec<_> = calls(&log).into_iter().map(|(a, _)| a).collect();
    assert_eq!(seen, [queue("th-3", "homeless")]);
    set("FAKE_QUEUE_STDERR", Some("nope"));
    let err = codex_notify_with_id("th-3", "homeless", None).unwrap_err().to_string();
    assert!(err.starts_with("codex delivery failed: cannot locate the codex home (CODEX_HOME and HOME unset); "), "{err}");
    set("HOME", saved_home.as_deref());
    set("CODEX_HOME", Some(home.to_str().unwrap()));
    calls(&log);

    // No codex on PATH: neither the daemon nor the queue can run.
    set("PATH", Some("/nonexistent-conde-bin"));
    let err = codex_notify_with_id("th-4", "hello", None).unwrap_err().to_string();
    assert!(err.starts_with("codex delivery failed: cannot start the codex app-server daemon: "), "{err}");
    assert!(err.contains("; codex queue fallback could not run: "), "{err}");
}
