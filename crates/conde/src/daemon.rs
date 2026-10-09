//! `conde daemon`: the unix socket the CLI talks to, plus the bridge.
//!
//! Local mode: the board and the bridge loop in one process, both through the
//! same dispatcher. Cloud mode (`--server`): the bridge only; every CLI
//! request is forwarded over the bridge's one authenticated WebSocket, and
//! the server's notifications are delivered into this host's sessions.

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use conde_remote::{BridgeClient, BridgeConfig, BridgeState, Endpoint, Notify, OutcomeReason};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::address::{Address, Kind};
use crate::bridge::Bridge;
use crate::credentials;
use crate::dispatch::{Dispatcher, Principal};
use crate::harness::{HarnessNotifier, doctor_checks};
use crate::notify::Notifier;
use crate::protocol::{ErrorCode, MAX_FRAME, Request, Response};
use crate::server;
use crate::util::{local_host, local_user, own_euid, same_user};

/// How often the cloud bridge rescans its sessions and reports the difference.
pub const ENDPOINT_RESCAN: Duration = Duration::from_secs(5);
/// How long in-flight requests may take to finish after SIGTERM.
const DRAIN: Duration = Duration::from_secs(20);

pub struct Options {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub interval: Duration,
    /// One bridge pass and exit (no server; local mode only).
    pub once: bool,
    /// Cloud mode: the server URL; the bridge only, the board is remote.
    pub server: Option<String>,
    /// Override of the notification retry backoff (local mode).
    pub retry_backoff: Option<Duration>,
    /// Override of the bridge client's idle timeout (cloud mode).
    pub idle_timeout: Option<Duration>,
}

pub fn run(opts: Options) -> anyhow::Result<()> {
    match opts.server.clone() {
        Some(server) => run_cloud(opts, &server),
        None => run_local(opts),
    }
}

/// Resolves on SIGTERM or SIGINT.
pub async fn termination() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot watch SIGTERM: {e}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM: shutting down"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT: shutting down"),
    }
}

enum Backend {
    Local { d: Arc<Dispatcher>, p: Principal },
    Remote { client: BridgeClient, url: String, host: String, credentials: PathBuf },
}

struct Daemon {
    backend: Backend,
}

impl Daemon {
    async fn handle(&self, req: Request) -> Response {
        let doctor = req.op == "doctor";
        let actor = req.actor.clone();
        let id = req.request_id.clone();
        let resp = match &self.backend {
            Backend::Local { d, p } => {
                let (d, p) = (d.clone(), p.clone());
                tokio::task::spawn_blocking(move || d.dispatch(&p, &req))
                    .await
                    .unwrap_or_else(|e| Response::err(&id, ErrorCode::Internal, format!("dispatch panicked: {e}")))
            }
            Backend::Remote { client, .. } => forward(client, &req, doctor).await,
        };
        if doctor { self.with_local_checks(resp, actor.as_deref()) } else { resp }
    }

    /// The board's doctor result plus this host's live checks.
    fn with_local_checks(&self, resp: Response, actor: Option<&str>) -> Response {
        let mut lines: Vec<Value> = match (&resp.result, &resp.error) {
            (Some(r), _) => r["checks"].as_array().cloned().unwrap_or_default(),
            (None, Some(e)) => vec![json!({"ok": false, "text": format!("board: {}", e.message)})],
            _ => Vec::new(),
        };
        let mut add = |ok: bool, text: String| lines.push(json!({"ok": ok, "text": text}));
        if let Backend::Remote { client, url, host, credentials } = &self.backend {
            match client.state() {
                BridgeState::Connected => add(
                    true,
                    format!(
                        "bridge: connected to {url} as host {host} (instance {}, {} connection(s) since start)",
                        client.bridge_instance(),
                        client.connects()
                    ),
                ),
                other => add(false, format!("bridge: not connected to {url} ({other:?})")),
            }
            match credentials::mode(credentials) {
                Some(0o600) => add(true, format!("credentials: {} mode 600", credentials.display())),
                Some(m) => add(false, format!("credentials: {} mode {m:03o}, expected 600", credentials.display())),
                None => add(false, format!("credentials: {} missing", credentials.display())),
            }
        }
        for (ok, text) in doctor_checks(&HarnessNotifier::new(None), actor) {
            add(ok, text);
        }
        let issues = lines.iter().filter(|l| l["ok"] == false).count();
        Response::ok(&resp.request_id, json!({"checks": lines, "issues": issues}))
    }
}

async fn forward(client: &BridgeClient, req: &Request, doctor: bool) -> Response {
    let unreachable = |why: String| {
        Response::err(&req.request_id, ErrorCode::Internal, format!("conde server unreachable through the bridge: {why}"))
    };
    let frame = match serde_json::to_value(req) {
        Ok(v) => v,
        Err(e) => return unreachable(e.to_string()),
    };
    // doctor must answer even while the bridge is down
    let wait = if doctor { Duration::from_secs(5) } else { Duration::from_secs(120) };
    match tokio::time::timeout(wait, client.call(frame)).await {
        Ok(Ok(v)) => serde_json::from_value(v).unwrap_or_else(|e| unreachable(format!("bad response frame: {e}"))),
        Ok(Err(e)) => unreachable(e.to_string()),
        Err(_) => unreachable(format!("no answer within {wait:?}")),
    }
}

/// The bound socket and the inode we own, so shutdown never unlinks a successor's.
struct Socket {
    listener: UnixListener,
    path: PathBuf,
    inode: (u64, u64),
}

fn bind(path: &Path) -> anyhow::Result<Socket> {
    let listener = server::bind(path)?;
    let m = std::fs::symlink_metadata(path)?;
    Ok(Socket { listener, path: path.to_path_buf(), inode: (m.dev(), m.ino()) })
}

fn unlink_own(path: &Path, inode: (u64, u64)) {
    if let Ok(m) = std::fs::symlink_metadata(path)
        && (m.dev(), m.ino()) == inode
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Serve CLI connections until `stop`; then stop accepting, unlink the
/// socket, let in-flight requests finish and close idle connections.
async fn serve(socket: Socket, daemon: Arc<Daemon>, mut stop: watch::Receiver<bool>) {
    let uid = own_euid();
    let mut conns = JoinSet::new();
    let conn_stop = stop.clone();
    loop {
        tokio::select! {
            _ = stop.wait_for(|s| *s) => break,
            accepted = socket.listener.accept() => match accepted {
                Ok((stream, _)) => match same_user(stream.peer_cred().map(|c| c.uid()), uid) {
                    Ok(()) => {
                        conns.spawn(connection(stream, daemon.clone(), conn_stop.clone()));
                    }
                    Err(why) => tracing::warn!("{why}"),
                },
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
    drop(socket.listener);
    unlink_own(&socket.path, socket.inode);
    if tokio::time::timeout(DRAIN, async { while conns.join_next().await.is_some() {} }).await.is_err() {
        tracing::warn!("requests still running after {DRAIN:?}; closing them");
    }
}

async fn connection(stream: UnixStream, daemon: Arc<Daemon>, mut stop: watch::Receiver<bool>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let frame = tokio::select! {
            f = server::read_frame(&mut reader, MAX_FRAME) => f,
            _ = stop.wait_for(|s| *s) => return,
        };
        let response = match frame {
            Ok(None) => return,
            Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Some(frame)) => match serde_json::from_slice::<Request>(&frame) {
                Ok(req) => daemon.handle(req).await,
                Err(e) => Response::err("", ErrorCode::BadRequest, format!("bad frame: {e}")),
            },
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let resp = Response::err("", ErrorCode::TooLarge, format!("frame exceeds {MAX_FRAME} bytes"));
                let _ = write_frame(&mut write, &resp).await;
                return;
            }
            Err(_) => return,
        };
        if write_frame(&mut write, &response).await.is_err() {
            return;
        }
    }
}

async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), resp: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await
}

/// Serve until SIGTERM / SIGINT or until `fatal` resolves (its error is returned).
async fn serve_until(
    socket: Socket,
    daemon: Arc<Daemon>,
    fatal: impl std::future::Future<Output = anyhow::Error>,
) -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let serving = serve(socket, daemon, stop_rx);
    tokio::pin!(serving);
    let result = tokio::select! {
        _ = &mut serving => Ok(()),
        _ = termination() => Ok(()),
        e = fatal => Err(e),
    };
    let _ = stop_tx.send(true);
    serving.await;
    result
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn run_local(opts: Options) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&opts.data_dir)
        .with_context(|| format!("creating {}", opts.data_dir.display()))?;
    let db_path = opts.data_dir.join("conde.db");
    let dispatcher = Arc::new(Dispatcher::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?);
    if let Some(b) = opts.retry_backoff {
        dispatcher.set_retry_ms(b.as_millis() as i64);
    }
    let principal = dispatcher.principal(&local_user(), &local_host(), true)?;
    let mut bridge =
        Bridge::new(dispatcher.clone(), dispatcher.bridge_principal(&principal), HarnessNotifier::new(None));
    if opts.once {
        return bridge.pass();
    }
    let runtime = runtime()?;
    let socket = runtime.block_on(async { bind(&opts.socket) })?;
    tracing::info!(
        "conde daemon (local): db={} socket={} owner={} host={} bridge={} interval={:?}",
        db_path.display(),
        opts.socket.display(),
        principal.user,
        principal.host,
        bridge.instance(),
        opts.interval
    );
    let stop = Arc::new(AtomicBool::new(false));
    let interval = opts.interval;
    let bridge_stop = stop.clone();
    let bridge_thread = std::thread::Builder::new().name("bridge".into()).spawn(move || {
        while !bridge_stop.load(Ordering::Relaxed) {
            if let Err(e) = bridge.pass() {
                tracing::error!("bridge pass failed: {e:#}");
            }
            let until = Instant::now() + interval;
            while !bridge_stop.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(interval.min(Duration::from_millis(50)));
            }
        }
        // dropping the bridge unlinks its Claude reply socket
    })?;
    let daemon = Arc::new(Daemon { backend: Backend::Local { d: dispatcher, p: principal } });
    let result = runtime.block_on(serve_until(socket, daemon, std::future::pending()));
    stop.store(true, Ordering::Relaxed);
    let _ = bridge_thread.join();
    tracing::info!("conde daemon stopped");
    result
}

fn endpoints(notifier: &HarnessNotifier) -> Vec<Endpoint> {
    notifier.live_claude().into_iter().map(|session_id| Endpoint::Claude { session_id }).collect()
}

fn run_cloud(opts: Options, server: &str) -> anyhow::Result<()> {
    let cred_path = credentials::path()?;
    let creds = credentials::load(&cred_path)?;
    if !credentials::same_server(&creds.server, server)? {
        bail!(
            "this host was enrolled with {}, not {server}; its key is never sent elsewhere (re-enroll with conde enroll --server {server})",
            creds.server
        );
    }
    let url = credentials::bridge_url(server)?;
    let runtime = runtime()?;
    let _rt = runtime.enter();
    // Bind first: a second daemon must fail here, before its hello fences the running bridge.
    let socket = bind(&opts.socket)?;
    let notifier = HarnessNotifier::new(None);
    let mut config = BridgeConfig::new(url.clone(), creds.key.clone(), creds.host_id.clone());
    config.endpoints = endpoints(&notifier);
    if let Some(t) = opts.idle_timeout {
        config.idle_timeout = t;
    }
    let (jobs_tx, jobs) = std::sync::mpsc::channel::<Notify>();
    let client = BridgeClient::start(config, move |n| {
        let _ = jobs_tx.send(n);
    });
    tracing::info!(
        "conde daemon (cloud): server={url} host={} bridge={} socket={}",
        creds.host_id,
        client.bridge_instance(),
        opts.socket.display()
    );
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (client, host, stop) = (client.clone(), creds.host_id.clone(), stop.clone());
        std::thread::Builder::new()
            .name("deliver".into())
            .spawn(move || deliver(notifier, client, jobs, host, stop, endpoints))?
    };
    let daemon = Arc::new(Daemon {
        backend: Backend::Remote { client: client.clone(), url, host: creds.host_id.clone(), credentials: cred_path },
    });
    let stopped = {
        let (client, stop) = (client.clone(), stop.clone());
        async move {
            let reason = client.stopped().await;
            // The delivery worker stops now, not after the CLI requests drain.
            stop.store(true, Ordering::Relaxed);
            match reason {
                conde_remote::StopReason::Fenced => {
                    anyhow!("fenced: a newer bridge for host took over (another daemon with this host's credential?)")
                }
                conde_remote::StopReason::Unauthorized => {
                    anyhow!("the server rejected this host's credential (revoked?); re-enroll with conde enroll")
                }
                other => anyhow!("bridge stopped: {other:?}"),
            }
        }
    };
    let result = runtime.block_on(serve_until(socket, daemon, stopped));
    client.shutdown();
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
    tracing::info!("conde daemon stopped");
    result
}

/// Whether the bridge client stopped for good (fenced, rejected, shut down).
fn bridge_stopped(client: &BridgeClient) -> bool {
    matches!(client.state(), BridgeState::Stopped(_))
}

/// The cloud bridge's delivery thread: owns the notifier (and so the Claude
/// reply socket), delivers each notify, reports outcomes and late receipts,
/// and reports session start/stop every `ENDPOINT_RESCAN`.
///
/// Admission is decided at the moment of delivery: once the bridge client has
/// stopped (a newer instance fenced it, or it shut down), no queued
/// notification is delivered any more, because the instance taking over
/// re-sends every attempt this one left open. A delivery already running when
/// the fence arrives completes; its outcome report is refused.
fn deliver<N: Notifier>(
    mut notifier: N,
    client: BridgeClient,
    jobs: std::sync::mpsc::Receiver<Notify>,
    host: String,
    stop: Arc<AtomicBool>,
    scan: impl Fn(&N) -> Vec<Endpoint>,
) {
    // A full outcome budget pauses delivery here until acks drain; the receipt is kept.
    let report = |attempt: &str, status: &str, reason: Option<&str>| loop {
        match client.report(attempt, status, reason) {
            Ok(_) => break,
            Err(conde_remote::CallError::Backpressure) if !stop.load(Ordering::Relaxed) && !bridge_stopped(&client) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            // Permanent: never retried as is. Settle the attempt as failed with a short reason;
            // if even that cannot be sent (an oversized attempt id) the server re-issues it.
            Err(e @ conde_remote::CallError::TooLarge { .. }) => {
                tracing::error!(attempt = %attempt.chars().take(80).collect::<String>(), status, "outcome too large to report: {e}");
                if status != "failed"
                    && let Err(e) = client.report(attempt, "failed", Some("outcome too large to report"))
                {
                    tracing::error!("attempt not settled: {e}");
                }
                break;
            }
            Err(e) => {
                tracing::warn!(attempt, status, "outcome not reported: {e}");
                break;
            }
        }
    };
    let mut last_scan = Instant::now();
    while !stop.load(Ordering::Relaxed) && !bridge_stopped(&client) {
        match jobs.recv_timeout(Duration::from_millis(200)) {
            Ok(n) if stop.load(Ordering::Relaxed) || bridge_stopped(&client) => {
                tracing::info!(attempt = n.attempt_id, "bridge stopped: queued notification left to the next instance");
                break;
            }
            Ok(n) => {
                let address = match &n.endpoint {
                    Endpoint::Claude { session_id } => Some(Address::new(Kind::Claude, session_id, &host)),
                    Endpoint::Codex { thread_id } => Some(Address::new(Kind::Codex, thread_id, &host)),
                    // A v1 daemon publishes nothing, so no mailbox is routed here.
                    Endpoint::Mailbox { .. } => None,
                };
                match address {
                    Some(address) => {
                        let result = notifier.notify(&address, &n.text, &n.attempt_id);
                        let (status, reason) = result.report();
                        report(&n.attempt_id, status, reason);
                    }
                    None => report(&n.attempt_id, "refused", Some(OutcomeReason::RecipientUnavailable.as_str())),
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for r in notifier.poll_receipts() {
            let (status, reason) = r.status.report();
            report(&r.attempt_id, status, reason);
        }
        if last_scan.elapsed() >= ENDPOINT_RESCAN {
            last_scan = Instant::now();
            client.set_endpoints(scan(&notifier));
        }
    }
    let dropped = jobs.try_iter().count();
    if dropped > 0 {
        tracing::info!(dropped, "queued notifications dropped unstarted");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};

    use conde_remote::{
        Authenticator, Dispatcher as RemoteDispatcher, FenceResult, NotificationSource, Outcome,
        Principal as RemotePrincipal, Recorded, StopReason,
    };

    use super::*;
    use crate::notify::{NotifyOutcome, Receipt};

    /// A server that accepts every bridge of host `box`; the newer of two
    /// connections fences the older one.
    struct Source;
    impl Authenticator for Source {
        fn authenticate(&self, _: &str) -> Option<RemotePrincipal> {
            Some(RemotePrincipal { user_id: "1".into(), host_id: "box".into() })
        }
    }
    impl RemoteDispatcher for Source {
        fn dispatch(&self, _: &RemotePrincipal, _: Value) -> Value {
            json!({"ok": true})
        }
    }
    impl NotificationSource for Source {
        fn pending_for_host(&self, _: &str, _: &str) -> Vec<Notify> {
            Vec::new()
        }
        fn record_outcome(&self, _: &str, _: &str, _: &Outcome) -> Recorded {
            Recorded::Accepted
        }
        fn fence(&self, _: &str, _: &str) -> FenceResult {
            FenceResult::Active
        }
    }

    /// Records each delivery; the delivery of `hold` blocks until released.
    struct Gate {
        hold: &'static str,
        started: Sender<()>,
        release: Receiver<()>,
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl Notifier for Gate {
        fn notify(&mut self, _: &Address, _: &str, attempt_id: &str) -> NotifyOutcome {
            self.seen.lock().unwrap().push(attempt_id.to_string());
            if attempt_id == self.hold {
                self.started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            NotifyOutcome::Accepted
        }
        fn poll_receipts(&mut self) -> Vec<Receipt> {
            Vec::new()
        }
    }

    fn job(attempt: &str) -> Notify {
        Notify {
            notification_id: format!("n-{attempt}"),
            attempt_id: attempt.into(),
            recipient: "u/r".into(),
            binding_version: 1,
            endpoint: Endpoint::Claude { session_id: "s".into() },
            message_id: format!("m-{attempt}"),
            text: "t".into(),
            fields: None,
        }
    }

    #[test]
    fn a_takeover_lets_the_running_delivery_finish_and_drops_queued_ones() {
        let rt = runtime().unwrap();
        let _guard = rt.enter();
        let source = Arc::new(Source);
        let server = rt
            .block_on(conde_remote::start(
                "127.0.0.1:0".parse().unwrap(),
                conde_remote::ServerConfig::default(),
                source.clone(),
                source.clone(),
                source,
            ))
            .unwrap();
        let config = || BridgeConfig::new(format!("ws://{}/v1/bridge", server.local_addr()), "k", "box");
        let old = BridgeClient::start(config(), |_| {});
        rt.block_on(old.call(json!({"op": "barrier"}))).unwrap();

        let (started_tx, started) = channel();
        let (release, release_rx) = channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let gate = Gate { hold: "running", started: started_tx, release: release_rx, seen: seen.clone() };
        // The client callback's queue stays open, as in the daemon.
        let (jobs_tx, jobs) = channel();
        jobs_tx.send(job("running")).unwrap();
        jobs_tx.send(job("queued")).unwrap();
        let (done_tx, done) = channel();
        let worker_client = old.clone();
        std::thread::spawn(move || {
            deliver(gate, worker_client, jobs, "box".into(), Arc::new(AtomicBool::new(false)), |_| Vec::new());
            done_tx.send(()).unwrap();
        });

        started.recv_timeout(Duration::from_secs(5)).expect("first delivery starts");
        let new = BridgeClient::start(config(), |_| {});
        assert_eq!(rt.block_on(old.stopped()), StopReason::Fenced);
        jobs_tx.send(job("after-fence")).unwrap();
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).expect("the worker stops with its client, not with the daemon");
        assert_eq!(*seen.lock().unwrap(), ["running"], "only the delivery admitted before the takeover ran");
        new.shutdown();
        rt.block_on(server.shutdown()).unwrap();
    }
}
