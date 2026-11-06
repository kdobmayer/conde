//! The sign-in relay on an agent machine (the relay seam contract): a board connection's `relay`
//! frames run the machine's helper `relay-<flow>`, one at a time, and every line it prints goes
//! back as a `relay_event`. `start` stops the running helper (stdin closed, then SIGTERM to its
//! process group) and spawns the new one with `start` on its stdin; `code` writes `code <code>`;
//! `cancel` closes its stdin (the helpers' EOF rule). A new connection or a stop of the board's
//! connection stops the running helper as well. The pasted code is written to the helper's stdin
//! and nowhere else. The machine's sign-in check (`agentvm-signin-check`) runs on connect, after a
//! relay's `done` and hourly; each flow it finds expiring or expired goes up as a `machine_event`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use conde_remote::RelayEvent;
use serde_json::{Value, json};

/// Where the machine image installs the helpers.
pub const HELPER_DIR: &str = "/usr/local/lib/agentvm";
/// How long a stopped helper may take to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The machine's sign-in check, in [`HELPER_DIR`]; it prints one line per flow.
pub const SIGNIN_CHECK: &str = "agentvm-signin-check";

/// Sends one `relay_event` up the board's connection.
pub type Emit = Arc<dyn Fn(&RelayEvent) + Send + Sync>;
/// Sends one `machine_event` up the board's connection.
pub type EmitMachine = Arc<dyn Fn(&Value) + Send + Sync>;

enum Cmd {
    Frame(conde_remote::Relay),
    /// A new connection: the helper of the old one is over.
    Reset,
    Stop,
}

/// One board connection's relay: an ordered worker, started by the first frame.
#[derive(Default)]
pub struct Relay {
    worker: Mutex<Option<mpsc::Sender<Cmd>>>,
}

impl Relay {
    /// Hands a `relay` frame to the worker, in frame order.
    pub fn frame(&self, frame: conde_remote::Relay, dir: &Path, emit: Emit) {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        let tx = worker.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let dir = dir.to_path_buf();
            std::thread::Builder::new()
                .name("conde-relay".into())
                .spawn(move || run(rx, dir, emit))
                .expect("spawn the relay worker");
            tx
        });
        let _ = tx.send(Cmd::Frame(frame));
    }

    /// A new connection: the running helper is stopped and its relay reported expired.
    pub fn reset(&self) {
        self.send(Cmd::Reset);
    }

    /// The board's connection stops: the running helper is stopped.
    pub fn stop(&self) {
        self.send(Cmd::Stop);
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self
            .worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = tx.send(cmd);
        }
    }
}

fn failed(relay_id: &str, reason: &str) -> RelayEvent {
    RelayEvent {
        relay_id: relay_id.to_string(),
        event: "failed".into(),
        url: None,
        user_code: None,
        expires_at: None,
        reason: Some(reason.into()),
    }
}

fn run(rx: mpsc::Receiver<Cmd>, dir: PathBuf, emit: Emit) {
    let mut current: Option<Helper> = None;
    for cmd in rx {
        match cmd {
            Cmd::Frame(f) => match f.action.as_str() {
                "start" => {
                    // a start resent after a failed clear is the relay already running
                    if current.as_ref().is_some_and(|h| h.relay_id == f.relay_id) {
                        continue;
                    }
                    if let Some(h) = current.take() {
                        h.stop();
                    }
                    current = spawn(&dir, &f, &emit);
                }
                "code" => match current.as_mut() {
                    Some(h) if h.relay_id == f.relay_id => {
                        let code = f.code.unwrap_or_default();
                        if code.contains(['\n', '\r']) {
                            emit(&failed(&f.relay_id, "bad_code"));
                        } else if h.write(&format!("code {code}\n")).is_err() {
                            tracing::warn!(relay = f.relay_id, "the sign-in helper took no code");
                        }
                    }
                    _ => emit(&failed(&f.relay_id, "process_exited")),
                },
                "cancel" => {
                    if let Some(h) = current.as_mut().filter(|h| h.relay_id == f.relay_id) {
                        h.cancel();
                    }
                }
                other => tracing::warn!(relay = f.relay_id, action = other, "unknown relay action"),
            },
            Cmd::Reset => {
                if let Some(h) = current.take() {
                    let id = h.relay_id.clone();
                    h.stop();
                    emit(&failed(&id, "expired"));
                }
            }
            Cmd::Stop => {
                if let Some(h) = current.take() {
                    h.stop();
                }
            }
        }
    }
    // the board connection is gone
    if let Some(h) = current {
        h.stop();
    }
}

/// A running helper: its stdin, and the child until the first of its reader and a stop takes it.
struct Helper {
    relay_id: String,
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Option<Child>>>,
    /// Cancelled or stopped: nothing it prints is reported any more.
    quiet: Arc<AtomicBool>,
}

impl Helper {
    fn write(&mut self, line: &str) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    fn cancel(&mut self) {
        self.quiet.store(true, Ordering::SeqCst);
        self.stdin = None;
    }

    fn stop(mut self) {
        self.cancel();
        let child = self.child.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(mut c) = child {
            terminate(&mut c);
        }
    }
}

/// SIGTERM to the helper's process group, SIGKILL after [`STOP_GRACE`].
fn terminate(c: &mut Child) {
    #[cfg(unix)]
    // SAFETY: kill(2) on the group this daemon spawned and has not reaped yet.
    unsafe {
        libc::kill(-(c.id() as i32), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = c.kill();
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if !matches!(c.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    #[cfg(unix)]
    // SAFETY: as above; the child is still unreaped.
    unsafe {
        libc::kill(-(c.id() as i32), libc::SIGKILL);
    }
    let _ = c.kill();
    let _ = c.wait();
}

/// Runs `relay-<flow>` from `dir` with `start` on its stdin; a flow without a helper (every
/// laptop) is `failed process_exited` at once.
fn spawn(dir: &Path, f: &conde_remote::Relay, emit: &Emit) -> Option<Helper> {
    let flow = f.flow.as_deref().unwrap_or_default();
    let path = dir.join(format!("relay-{flow}"));
    if !crate::ops::machines::RELAY_FLOWS.contains(&flow) || !path.is_file() {
        tracing::warn!(
            relay = f.relay_id,
            flow,
            "no sign-in helper {}",
            path.display()
        );
        emit(&failed(&f.relay_id, "process_exited"));
        return None;
    }
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(relay = f.relay_id, flow, "sign-in helper not started: {e}");
            emit(&failed(&f.relay_id, "process_exited"));
            return None;
        }
    };
    tracing::info!(relay = f.relay_id, flow, "sign-in helper started");
    let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
    let mut helper = Helper {
        relay_id: f.relay_id.clone(),
        stdin,
        child: Arc::new(Mutex::new(Some(child))),
        quiet: Arc::default(),
    };
    if let Some(stdout) = stdout {
        let (id, child, quiet, emit) = (
            helper.relay_id.clone(),
            helper.child.clone(),
            helper.quiet.clone(),
            emit.clone(),
        );
        std::thread::Builder::new()
            .name("conde-relay-helper".into())
            .spawn(move || report(stdout, &id, &child, &quiet, &emit))
            .expect("spawn the relay reader");
    }
    if helper.write("start\n").is_err() {
        tracing::warn!(relay = helper.relay_id, "the sign-in helper took no start");
    }
    Some(helper)
}

/// Reports the helper's lines until it closes its output, then reaps it; an exit without `done`
/// or `failed` is `failed process_exited`.
fn report(
    stdout: std::process::ChildStdout,
    relay_id: &str,
    child: &Mutex<Option<Child>>,
    quiet: &AtomicBool,
    emit: &Emit,
) {
    let mut over = false;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Some(event) = parse(relay_id, &line) else {
            tracing::warn!(relay = relay_id, "unrecognized sign-in helper line");
            continue;
        };
        if quiet.load(Ordering::SeqCst) {
            continue;
        }
        over |= matches!(event.event.as_str(), "done" | "failed");
        emit(&event);
    }
    let taken = child.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(mut c) = taken {
        let _ = c.wait();
    }
    if !over && !quiet.load(Ordering::SeqCst) {
        emit(&failed(relay_id, "process_exited"));
    }
}

/// One helper line as its event: `url <URL> [<user_code> <expires_at>]`, `waiting`, `done`,
/// `failed <reason>`.
fn parse(relay_id: &str, line: &str) -> Option<RelayEvent> {
    let mut words = line.split_whitespace();
    let mut event = RelayEvent {
        relay_id: relay_id.to_string(),
        event: words.next()?.to_string(),
        url: None,
        user_code: None,
        expires_at: None,
        reason: None,
    };
    let rest: Vec<&str> = words.collect();
    match (event.event.as_str(), rest.as_slice()) {
        ("url", [url]) => event.url = Some(url.to_string()),
        ("url", [url, code, expires]) => {
            event.url = Some(url.to_string());
            event.user_code = Some(code.to_string());
            event.expires_at = Some(expires.to_string());
        }
        ("waiting" | "done", []) => {}
        ("failed", [reason]) => event.reason = Some(reason.to_string()),
        _ => return None,
    }
    Some(event)
}

/// One board connection's sign-in check: a worker of its own that runs the check when asked and
/// `every` after its last run, started only where the machine has the script.
#[derive(Default)]
pub struct SignInCheck {
    worker: Mutex<Option<mpsc::Sender<()>>>,
}

impl SignInCheck {
    /// Checks now, starting the worker when `dir` has the script (a new connection).
    pub fn start(&self, dir: &Path, every: Duration, emit: EmitMachine) {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_none() {
            let path = dir.join(SIGNIN_CHECK);
            if !path.is_file() {
                return;
            }
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("conde-signin-check".into())
                .spawn(move || check_loop(&rx, &path, every, &emit))
                .expect("spawn the sign-in check");
            *worker = Some(tx);
        }
        if let Some(tx) = worker.as_ref() {
            let _ = tx.send(());
        }
    }

    /// Checks now if the worker runs (a relay ended `done`).
    pub fn now(&self) {
        if let Some(tx) = self
            .worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = tx.send(());
        }
    }

    /// The board's connection stops: the worker ends after its running check.
    pub fn stop(&self) {
        self.worker.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
}

fn check_loop(rx: &mpsc::Receiver<()>, path: &Path, every: Duration, emit: &EmitMachine) {
    loop {
        match rx.recv_timeout(every) {
            Ok(()) => while rx.try_recv().is_ok() {},
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if let Some(out) = run_check(path) {
            for event in sign_in_events(&out) {
                emit(&event);
            }
        }
    }
}

/// The check's output; None when it did not start.
fn run_check(path: &Path) -> Option<String> {
    match Command::new(path)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(out) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) => {
            tracing::warn!("sign-in check not started: {e}");
            None
        }
    }
}

/// The check's lines (`claude ok|expiring|expired|unknown <at>`, `github ok|expired`, `codex
/// ok|expired`) as the contract's `needs_sign_in` events: one per flow that is expiring or
/// expired, each with every flow's state; none when no flow needs the person.
fn sign_in_events(out: &str) -> Vec<Value> {
    let mut flows = serde_json::Map::new();
    for line in out.lines() {
        let mut words = line.split_whitespace();
        if let (Some(flow), Some(state)) = (words.next(), words.next())
            && crate::ops::machines::RELAY_FLOWS.contains(&flow)
        {
            flows.insert(flow.to_string(), json!(state));
        }
    }
    flows
        .iter()
        .filter(|(_, state)| *state == "expiring" || *state == "expired")
        .map(|(flow, _)| json!({"state": "needs_sign_in", "flow": flow, "flows": flows}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_lines_are_the_contract_events() {
        let e = parse("r", "url https://claude.com/cai/oauth/authorize?x=1").unwrap();
        assert_eq!(
            (e.event.as_str(), e.url.as_deref(), e.user_code),
            (
                "url",
                Some("https://claude.com/cai/oauth/authorize?x=1"),
                None
            )
        );
        let e = parse(
            "r",
            "url https://github.com/login/device WDJB-MJHT 2026-11-06T10:15:00Z",
        )
        .unwrap();
        assert_eq!(e.user_code.as_deref(), Some("WDJB-MJHT"));
        assert_eq!(e.expires_at.as_deref(), Some("2026-11-06T10:15:00Z"));
        assert_eq!(parse("r", "waiting").unwrap().event, "waiting");
        assert_eq!(
            parse("r", "failed bad_code").unwrap().reason.as_deref(),
            Some("bad_code")
        );
        assert!(parse("r", "done now").is_none());
        assert!(parse("r", "hello").is_none());
        assert!(parse("r", "").is_none());
    }

    #[test]
    fn check_lines_are_one_needs_sign_in_per_flow_that_needs_the_person() {
        let out = "claude expiring 2026-11-08T10:00:00Z\ngithub expired\ncodex ok\n";
        let flows = json!({"claude": "expiring", "github": "expired", "codex": "ok"});
        assert_eq!(
            sign_in_events(out),
            [
                json!({"state": "needs_sign_in", "flow": "claude", "flows": flows}),
                json!({"state": "needs_sign_in", "flow": "github", "flows": flows}),
            ]
        );
        let ok = "claude ok 2026-12-01T00:00:00Z\ngithub ok\ncodex ok\n";
        assert!(sign_in_events(ok).is_empty());
        assert!(sign_in_events("claude unknown -\ngithub ok\ncodex ok\n").is_empty());
        assert!(sign_in_events("").is_empty());
    }
}
