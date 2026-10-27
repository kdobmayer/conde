//! `conde daemon`: one shape (DESIGN-v2.md §5). The local board (SQLite, the unix socket the
//! CLI and MCP talk to, local delivery) plus one connection per enrolled shared board. Every
//! request goes through the resolver; a request resolved to local is dispatched exactly as v1
//! local mode and never waits on a board. A board that is fenced, revoked or keeps failing
//! stops alone.

mod board;
pub mod legacy;
pub mod mux;
mod publish;
pub mod resolve;

use std::collections::BTreeMap;
use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinSet;

pub use self::board::Status as BoardStatus;
use self::board::{BoardConn, Status};
pub use self::mux::{OriginStats, Tuning};
use self::mux::{Delivery, Mux};
use self::resolve::{Target, order_key, qualify, resolve};
use crate::address::{Address, Kind};
use crate::boards;
use crate::bridge::Bridge;
use crate::dispatch::{Dispatcher, Principal};
use crate::harness::{HarnessNotifier, doctor_checks};
use crate::notify::Notifier;
use crate::protocol::v2::{BoardPart, LOCAL_BOARD, label, op};
use crate::protocol::{ErrorCode, MAX_FRAME, Request, Response};
use crate::ipc;
use crate::server;
use crate::store::Tx;
use crate::store::local::{BoardOpKind, LocalTx, PublicationState};
use crate::store::sqlite::SqliteTx;
use crate::util::{local_host, local_user};

/// How often sessions are rescanned (endpoint reporting, session replacement).
pub const ENDPOINT_RESCAN: Duration = Duration::from_secs(5);
/// How long in-flight requests may take to finish after SIGTERM.
const DRAIN: Duration = Duration::from_secs(20);

pub type Refusal = (ErrorCode, String);

pub struct Options {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    /// `$XDG_CONFIG_HOME/conde` (board files, default board) when `None`.
    pub config_dir: Option<PathBuf>,
    /// Claude's config dir (`$CLAUDE_CONFIG_DIR` or `~/.claude` when `None`).
    pub claude_config: Option<PathBuf>,
    pub interval: Duration,
    /// One local delivery pass and exit (no socket, no boards).
    pub once: bool,
    /// The v1 `--server`: accepted and ignored with a warning for one release.
    pub server: Option<String>,
    /// The v1 bridge (`--v1-bridge <url>`): kept for mixed v1/v2 boards until the last host
    /// migrates; never started by `conde setup`.
    pub v1_bridge: Option<String>,
    /// Override of the local notification retry backoff.
    pub retry_backoff: Option<Duration>,
    /// Override of the board clients' idle timeout.
    pub idle_timeout: Option<Duration>,
    /// This host and its OS user (default: the machine's).
    pub host: Option<String>,
    pub user: Option<String>,
    pub tuning: Tuning,
}

impl Options {
    pub fn new(data_dir: PathBuf, socket: PathBuf) -> Options {
        Options {
            data_dir,
            socket,
            config_dir: None,
            claude_config: None,
            interval: Duration::from_secs(1),
            once: false,
            server: None,
            v1_bridge: None,
            retry_backoff: None,
            idle_timeout: None,
            host: None,
            user: None,
            tuning: Tuning::default(),
        }
    }
}

/// `conde daemon`: serves until SIGTERM or SIGINT.
pub fn run(opts: Options) -> anyhow::Result<()> {
    if let Some(url) = opts.v1_bridge.clone() {
        return legacy::run(opts, &url);
    }
    if let Some(server) = &opts.server {
        tracing::warn!(
            "--server {server} is ignored: the daemon runs the local board and connects to the boards in \
             conde/boards/ (conde setup rewrites the service)"
        );
    }
    let notifier = HarnessNotifier::new(opts.claude_config.clone());
    if opts.once {
        let (d, p) = open_local(&opts)?;
        let mut bridge = Bridge::new(d.clone(), d.bridge_principal(&p), notifier);
        return bridge.pass();
    }
    let runtime = runtime()?;
    let daemon = runtime.block_on(async { Daemon::start(&opts, notifier) })?;
    runtime.block_on(daemon.serve(termination()));
    drop(runtime);
    tracing::info!("conde daemon stopped");
    Ok(())
}

/// Resolves on Ctrl-C, Ctrl-Break, closing the console or system shutdown. A
/// scheduled task's End is a hard stop that this never sees.
#[cfg(windows)]
pub async fn termination() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};
    let (Ok(mut brk), Ok(mut close), Ok(mut shutdown)) = (ctrl_break(), ctrl_close(), ctrl_shutdown()) else {
        tracing::error!("cannot watch the console control events");
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C: shutting down"),
        _ = brk.recv() => tracing::info!("Ctrl-Break: shutting down"),
        _ = close.recv() => tracing::info!("console closed: shutting down"),
        _ = shutdown.recv() => tracing::info!("system shutdown: shutting down"),
    }
}

/// Resolves on SIGTERM or SIGINT.
#[cfg(unix)]
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

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn open_local(opts: &Options) -> anyhow::Result<(Arc<Dispatcher>, Principal)> {
    crate::private::create_dir(&opts.data_dir).with_context(|| format!("creating {}", opts.data_dir.display()))?;
    let db_path = opts.data_dir.join("conde.db");
    let d = Arc::new(Dispatcher::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?);
    if let Some(b) = opts.retry_backoff {
        d.set_retry_ms(b.as_millis() as i64);
    }
    let user = opts.user.clone().unwrap_or_else(local_user);
    let host = opts.host.clone().unwrap_or_else(local_host);
    let p = d.principal(&user, &host, true)?;
    Ok((d, p))
}

/// The daemon's state, shared by the request handlers, the board tasks and the delivery thread.
pub struct Core {
    pub(crate) local: Arc<Dispatcher>,
    pub(crate) principal: Principal,
    pub(crate) mux: Arc<Mux>,
    boards: Mutex<BTreeMap<String, Arc<BoardConn>>>,
    /// Held by [`reload_board`] and `board.forget`: one replacement of a board's connection at
    /// a time, so two reloads never leave a second, untracked connection.
    pub(crate) reloads: Mutex<()>,
    pub(crate) config_dir: PathBuf,
    pub(crate) db_path: PathBuf,
    claude_config: Option<PathBuf>,
    pub(crate) rt: tokio::runtime::Handle,
    /// The v1 credential file's state, while it is not moved into `boards/` (doctor).
    legacy: Mutex<Option<String>>,
}

impl Core {
    /// One write transaction on the local board with its own tables.
    pub(crate) fn local_tx<T>(&self, f: impl FnOnce(&SqliteTx) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let sqlite = self.local.sqlite().context("the local board is SQLite")?;
        let tx = sqlite.begin_local()?;
        let out = f(&tx)?;
        Tx::commit(tx)?;
        Ok(out)
    }

    pub(crate) fn board(&self, alias: &str) -> Option<Arc<BoardConn>> {
        self.boards.lock().unwrap_or_else(|p| p.into_inner()).get(alias).cloned()
    }

    pub(crate) fn boards(&self) -> Vec<Arc<BoardConn>> {
        self.boards.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    fn add_board(&self, conn: Arc<BoardConn>) {
        self.boards.lock().unwrap_or_else(|p| p.into_inner()).insert(conn.alias.clone(), conn);
    }

    /// `conn` is the board's connection, not one a reload replaced (or a forgotten one).
    pub(crate) fn is_current(&self, conn: &Arc<BoardConn>) -> bool {
        self.board(&conn.alias).is_some_and(|c| Arc::ptr_eq(&c, conn))
    }

    pub(crate) fn remove_board(&self, alias: &str) {
        self.boards.lock().unwrap_or_else(|p| p.into_inner()).remove(alias);
    }

    pub(crate) fn default_board(&self) -> String {
        boards::default_in(&self.config_dir)
    }

    /// Whether a session behind `a` is live, for the migration's conflict rule.
    pub(crate) fn live(&self, a: &Address) -> bool {
        match a.kind {
            Kind::Claude => conde_transport::discover_claude(self.claude_config.as_deref()).iter().any(|s| s.session_id == a.target),
            _ => true,
        }
    }

    pub(crate) fn board_state(&self, conn: &BoardConn) -> String {
        match conn.status() {
            Status::NotMigrated => "not migrated".into(),
            Status::Stopped(why) | Status::Broken(why) => why,
            Status::Running => match conn.client().map(|c| c.state()) {
                Some(conde_remote::BridgeState::Connected) if self.mux.is_reconciled(&conn.alias) => "connected".into(),
                Some(conde_remote::BridgeState::Connected) => "connected (reconciling)".into(),
                Some(conde_remote::BridgeState::Backoff) => "unreachable (reconnecting)".into(),
                _ => "connecting".into(),
            },
        }
    }

    /// The local agent a request acts as: `--from` for the human, else the one name bound to
    /// the caller's address (the v1 rule).
    pub(crate) fn local_identity(&self, req: &Request) -> Result<crate::store::Agent, Refusal> {
        let raw = req
            .actor
            .as_deref()
            .ok_or_else(|| (ErrorCode::Unauthenticated, "this request needs the caller's identity (actor)".to_string()))?;
        let address = Address::parse_v1(raw, &self.principal.host).map_err(|e| (ErrorCode::BadRequest, e.to_string()))?;
        if address.host != self.principal.host {
            return Err((ErrorCode::Forbidden, format!("actor host '{}' is not this host", address.host)));
        }
        let owner = self.principal.user_id;
        let found = self.local.with_conn(|tx| match &req.from {
            Some(from) if address.kind == Kind::Human => {
                let name = crate::address::normalize_name(from).unwrap_or_default();
                Ok(tx.agent_by_name(owner, &name)?.filter(|a| a.address.is_some()).into_iter().collect())
            }
            _ => tx.agents_at_address(owner, &address),
        });
        let mut found = found.map_err(|e| (ErrorCode::Internal, e.to_string()))?;
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err((
                ErrorCode::Unauthenticated,
                crate::dispatch::unregistered(&address),
            )),
            _ => Err((ErrorCode::Conflict, "multiple registry names point at this session".to_string())),
        }
    }
}

/// A running daemon: the local board, its delivery thread and its board connections.
pub struct Daemon {
    core: Arc<Core>,
    socket: Socket,
    stop_delivery: Arc<AtomicBool>,
    delivery: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// Binds the socket, opens the local board and starts local delivery before any network
    /// call; then starts each migrated board's connection. Needs a tokio runtime context.
    pub fn start<N: Notifier + Send + 'static>(opts: &Options, notifier: N) -> anyhow::Result<Daemon> {
        // Bind first: a second daemon must fail here, before it claims or connects anything.
        let socket = bind(&opts.socket)?;
        let (local, principal) = open_local(opts)?;
        let config_dir = match &opts.config_dir {
            Some(d) => d.clone(),
            None => boards::config_dir()?,
        };
        let mut tuning = opts.tuning.clone();
        if tuning.idle_timeout.is_none() {
            tuning.idle_timeout = opts.idle_timeout;
        }
        let mux = Arc::new(Mux::new(tuning));
        let core = Arc::new(Core {
            local: local.clone(),
            principal: principal.clone(),
            mux: mux.clone(),
            boards: Mutex::default(),
            reloads: Mutex::default(),
            config_dir,
            db_path: opts.data_dir.join("conde.db"),
            claude_config: opts.claude_config.clone(),
            rt: tokio::runtime::Handle::current(),
            legacy: Mutex::default(),
        });
        let stop_delivery = Arc::new(AtomicBool::new(false));
        let delivery = Delivery {
            mux,
            local: local.clone(),
            bridge: local.bridge_principal(&principal),
            interval: opts.interval,
            pending: Mutex::default(),
        };
        let stop = stop_delivery.clone();
        let delivery = std::thread::Builder::new().name("deliver".into()).spawn(move || delivery.run(notifier, stop))?;
        tracing::info!(
            "conde daemon: db={} socket={} owner={} host={} interval={:?}",
            core.db_path.display(),
            opts.socket.display(),
            principal.user,
            principal.host,
            opts.interval
        );
        for (alias, config) in boards::load_all(&core.config_dir) {
            open_board(&core, &alias, config);
        }
        if boards::legacy_path(&core.config_dir).exists() {
            core.rt.spawn(adopt_legacy(core.clone()));
        }
        Ok(Daemon { core, socket, stop_delivery, delivery: Some(delivery) })
    }

    pub fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// Serves the socket until `until` resolves, then stops delivery and every board.
    pub async fn serve(mut self, until: impl Future<Output = ()>) {
        let (stop_tx, stop_rx) = watch::channel(false);
        let handler: Arc<dyn Handler> = self.core.clone();
        let socket = std::mem::replace(&mut self.socket, Socket::none());
        let serving = serve(socket, handler, stop_rx);
        tokio::pin!(serving);
        tokio::select! {
            _ = &mut serving => {}
            _ = until => {}
        }
        let _ = stop_tx.send(true);
        serving.await;
        self.stop();
    }

    /// Stops delivery and every board connection at once (what a process exit does to them:
    /// retained jobs and unacknowledged outcomes are gone).
    fn stop(&mut self) {
        for conn in self.core.boards() {
            conn.shutdown();
        }
        self.stop_delivery.store(true, Ordering::Relaxed);
        if let Some(t) = self.delivery.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Registers one board file: connects it when its migration is done, resumes a pending one.
fn open_board(core: &Arc<Core>, alias: &str, config: anyhow::Result<boards::BoardConfig>) {
    let conn = Arc::new(BoardConn::new(alias, config));
    core.add_board(conn.clone());
    activate(core, &conn);
}

/// What [`reload_board`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reloaded {
    /// A board this daemon did not have: opened as at start.
    Opened,
    /// The connection was replaced by one with the file's credential.
    Replaced,
    /// The file holds what the running connection uses.
    Unchanged,
}

/// Re-reads `boards/<alias>.json` (written by `conde login` or `conde enroll`) and replaces the
/// board's connection with one that uses it, as a restart would.
///
/// `refused`: the reread after the board refused that connection's key; it replaces only that
/// connection, only when the file holds another credential. Otherwise (`board.reload`) a file
/// that does not load is refused and the running connection kept; an unchanged file reopens a
/// stopped connection and leaves a live one alone.
pub(crate) fn reload_board(core: &Arc<Core>, alias: &str, refused: Option<&Arc<BoardConn>>) -> anyhow::Result<Reloaded> {
    let _one = core.reloads.lock().unwrap_or_else(|p| p.into_inner());
    let old = core.board(alias);
    if let Some(r) = refused
        && !old.as_ref().is_some_and(|o| Arc::ptr_eq(o, r))
    {
        // another reload replaced it already
        return Ok(Reloaded::Unchanged);
    }
    let path = boards::path_in(&core.config_dir, alias);
    if !path.exists() {
        anyhow::bail!("no board file {}", path.display());
    }
    let config = match boards::load(&path) {
        Err(e) if refused.is_none() => return Err(e.context("the running connection is kept")),
        c => c,
    };
    if let Some(old) = &old {
        let unchanged = matches!((&old.config, &config), (Some(a), Ok(b)) if a == b);
        let live = !matches!(old.status(), Status::Stopped(_) | Status::Broken(_));
        if unchanged && (live || refused.is_some()) {
            return Ok(Reloaded::Unchanged);
        }
    }
    if let Some(d) = core.mux.tuning.reload_delay {
        std::thread::sleep(d);
    }
    let conn = Arc::new(BoardConn::new(alias, config));
    // registered first: from here the old connection's tasks and callbacks apply nothing
    core.add_board(conn.clone());
    if let Some(old) = &old {
        old.set_status(Status::Stopped("replaced by a reload".into()));
        if let Some(client) = old.client() {
            client.shutdown();
            core.mux.stop_client(alias, &client, "reloaded");
        }
    }
    tracing::info!(board = alias, "board file reloaded");
    activate(core, &conn);
    Ok(if old.is_some() { Reloaded::Replaced } else { Reloaded::Opened })
}

/// Connects a registered board, or waits for its migration as at start.
fn activate(core: &Arc<Core>, conn: &Arc<BoardConn>) {
    let alias = conn.alias.as_str();
    if conn.config.is_none() {
        return;
    }
    match core.local_tx(|tx| Ok(tx.migrate_op(alias)?)) {
        Ok(Some(op)) if op.state == crate::store::local::BoardOpState::Done => board::start(core, conn),
        Ok(Some(op)) => {
            core.rt.spawn(board::resume_migration(core.clone(), conn.clone(), op));
        }
        Ok(None) => {
            tracing::info!(board = alias, "not migrated: no connection until conde setup confirms it");
            core.rt.spawn(board::probe_migrated(Arc::downgrade(core), conn.clone()));
        }
        Err(e) => tracing::error!(board = alias, "cannot read the board's ops: {e:#}"),
    }
}

/// Moves the v1 credential into `boards/<alias>.json` once the board names itself and the
/// account (`board.info`, over HTTPS; no hello).
async fn adopt_legacy(core: Arc<Core>) {
    let path = boards::legacy_path(&core.config_dir);
    let mut delay = Duration::from_secs(1);
    loop {
        let set = |s: String| *core.legacy.lock().unwrap_or_else(|p| p.into_inner()) = Some(s);
        let creds = match crate::credentials::load(&path) {
            Ok(c) => c,
            Err(e) => {
                set(format!("{e:#}"));
                return;
            }
        };
        let info = async {
            let url = crate::credentials::rpc_url(&creds.server).map_err(|e| format!("{e:#}"))?;
            let request = json!({"op": crate::protocol::v2::op::BOARD_INFO, "body": {}});
            let v = conde_remote::rpc(&url, &creds.key, request, core.mux.tuning.call_timeout).await.map_err(|e| e.to_string())?;
            let resp: Response = serde_json::from_value(v).map_err(|e| e.to_string())?;
            match (resp.result, resp.error) {
                (Some(r), None) => serde_json::from_value::<crate::protocol::v2::BoardInfo>(r).map_err(|e| e.to_string()),
                (_, Some(e)) => Err(e.message),
                _ => Err("empty response".into()),
            }
        }
        .await;
        match info {
            Ok(info) => {
                let alias = info.name.clone();
                match boards::adopt_legacy(&core.config_dir, &alias, &info.owner) {
                    Ok(p) => {
                        tracing::info!(board = alias, file = %p.display(), "v1 credential moved");
                        *core.legacy.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        open_board(&core, &alias, boards::load(&boards::path_in(&core.config_dir, &alias)));
                    }
                    Err(e) => set(format!("{} not moved: {e:#}", path.display())),
                }
                return;
            }
            Err(why) => set(format!("{} not moved yet: board.info: {why}", path.display())),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(300));
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What answers the socket's requests.
pub(crate) trait Handler: Send + Sync + 'static {
    fn handle(self: Arc<Self>, req: Request) -> BoxFuture<'static, Response>;
}

impl Handler for Core {
    fn handle(self: Arc<Self>, req: Request) -> BoxFuture<'static, Response> {
        Box::pin(async move { self.answer(req).await })
    }
}

/// The name an op looks up on the board it resolved to; a local miss names the boards.
fn looks_up_names(op: &str, body: &Value) -> Option<String> {
    let name = match op {
        "agent.resolve" | "profile.get" | "profile.set" | "channel.invite" => body.get("name"),
        "message.send" => body.pointer("/to/agent"),
        _ => None,
    };
    name.and_then(Value::as_str).map(str::to_string)
}

/// Directory reads a caller may make on a shared board without a publication.
fn directory_read(op: &str) -> bool {
    matches!(op, "agent.list" | "agent.resolve" | "profile.get" | "channel.list" | "channel.members")
}

impl Core {
    async fn answer(self: Arc<Self>, req: Request) -> Response {
        let id = req.request_id.clone();
        let done = |r: Result<Value, Refusal>| match r {
            Ok(v) => Response::ok(&id, v),
            Err((code, message)) => Response::err(&id, code, message),
        };
        match req.op.as_str() {
            op::MAILBOX_PUBLISH => done(publish::publish(&self, &req).await),
            op::MAILBOX_UNPUBLISH => done(publish::unpublish(&self, &req).await),
            crate::surface::BOARD_LIST => done(publish::list(&self)),
            crate::surface::BOARD_FORGET => done(publish::forget(&self, &req)),
            crate::surface::BOARD_RELOAD => done(publish::reload(&self, &req)),
            op::BOARD_INFO => done(publish::board_info(&self, &req).await),
            op::HOST_MIGRATE_PREVIEW => done(publish::migrate_preview(&self, &req).await),
            op::HOST_MIGRATE_V2 => done(publish::migrate(&self, &req).await),
            _ => self.route(req).await,
        }
    }

    async fn route(self: &Arc<Self>, req: Request) -> Response {
        let id = req.request_id.clone();
        let default = self.default_board();
        let enrolled = self.enrolled();
        let (target, req) = match resolve(&req, &default, &enrolled) {
            Ok(r) => r,
            Err((code, message)) => return Response::err(&id, code, message),
        };
        match (req.op.as_str(), target) {
            ("doctor", target) => self.doctor(req, target).await,
            ("agent.whoami", Target::Local | Target::Aggregate) => self.whoami(req).await,
            ("agent.register" | "agent.rename", Target::Local) => {
                let register = req.op == "agent.register";
                let mut resp = self.local(req).await;
                if let Some(result) = resp.result.as_mut() {
                    let name = result["name"].as_str().unwrap_or_default();
                    let short = name.rsplit('/').next().unwrap_or(name).to_string();
                    let agent = self.local.with_conn(|tx| tx.agent_by_name(self.principal.user_id, &short)).ok().flatten();
                    result["publications"] = Value::Array(agent.map(|a| self.publications_of(&a.id)).unwrap_or_default());
                    if register {
                        result["boards"] = json!(enrolled_names(self));
                    }
                }
                resp
            }
            (_, Target::Local) => self.local(req).await,
            (_, Target::Board(alias)) => self.shared(&alias, req).await,
            (_, Target::Aggregate) => self.aggregate(req).await,
        }
    }

    /// A request resolved to the local board: dispatched as v1 local mode.
    async fn local(&self, req: Request) -> Response {
        let (d, p) = (self.local.clone(), self.principal.clone());
        let id = req.request_id.clone();
        let names = looks_up_names(&req.op, &req.body);
        let mut resp = tokio::task::spawn_blocking(move || d.dispatch(&p, &req))
            .await
            .unwrap_or_else(|e| Response::err(&id, ErrorCode::Internal, format!("dispatch panicked: {e}")));
        if let Some(name) = names
            && !self.boards().is_empty()
            && let Some(e) = resp.error.as_mut()
            && e.code == ErrorCode::NotFound
            && e.message.starts_with("no agent")
        {
            e.message = crate::surface::no_agent(&name, LOCAL_BOARD);
        }
        resp
    }

    /// The `actor` a shared request carries: the caller's mailbox (its human address for a
    /// human); `None` for a directory read by an unpublished caller.
    fn shared_actor(&self, conn: &BoardConn, req: &Request) -> Result<Option<String>, Refusal> {
        let directory = directory_read(&req.op);
        let me = match self.local_identity(req) {
            Ok(me) => me,
            Err(_) if directory => return Ok(None),
            Err(e) => return Err(e),
        };
        // A human's watch reads as the owner the host's credential names, published or not
        // (the server scopes it to their agents); every other call needs a published name.
        if me.is_human() && (req.op == "message.feed" || req.op == "agent.retire") {
            return Ok(Some(format!("human:{}@{}", conn.owner(), conn.host())));
        }
        let alias = &conn.alias;
        let publication = board::publication_of(self, alias, &me.id);
        match publication {
            Some(p) if p.state == PublicationState::Published => {
                let host = conn.host();
                Ok(Some(if me.is_human() {
                    format!("human:{}@{host}", conn.owner())
                } else {
                    format!("mailbox:{}@{host}", p.mailbox_id.unwrap_or_default())
                }))
            }
            _ if directory => Ok(None),
            Some(p) => Err((ErrorCode::Forbidden, crate::surface::publication_pending(&me.name, alias, p.state.as_str()))),
            None => Err((ErrorCode::Forbidden, crate::surface::not_published(&me.name, alias))),
        }
    }

    /// A request resolved to one shared board: translated (actor, no `from`), forwarded, and
    /// its ids qualified with the alias.
    async fn shared(&self, alias: &str, req: Request) -> Response {
        let id = req.request_id.clone();
        let Some(conn) = self.board(alias) else {
            return Response::err(&id, ErrorCode::NotFound, format!("no board '{alias}' on this host"));
        };
        // Before `--from` makes the human act as an agent: a human never sends on a shared board.
        let human = req.actor.as_deref().is_some_and(|a| {
            Address::parse_v1(a, &self.principal.host).is_ok_and(|x| x.kind == Kind::Human)
        });
        if human && crate::humans::SEND_OPS.contains(&req.op.as_str()) {
            return Response::err(&id, ErrorCode::Forbidden, crate::humans::no_sending(alias));
        }
        let actor = match self.shared_actor(&conn, &req) {
            Ok(a) => a,
            Err((code, message)) => return Response::err(&id, code, message),
        };
        forward(&conn, &req, actor, self.mux.tuning.call_timeout).await
    }

    /// The boards whose part an aggregate view needs: connected ones within the bounded wait,
    /// the others unavailable at once.
    async fn shared_parts(&self, req: &Request) -> Vec<(String, Option<Value>)> {
        let Ok(me) = self.local_identity(req) else { return Vec::new() };
        let wait = self.mux.tuning.aggregate_wait;
        let deadline = tokio::time::Instant::now() + wait;
        let read_all = req.op == "message.read";
        let mut calls = JoinSet::new();
        let mut parts = Vec::new();
        for conn in self.boards() {
            let published = board::publication_of(self, &conn.alias, &me.id).filter(|p| p.state == PublicationState::Published);
            let Some(p) = published else { continue };
            if !conn.connected() || !self.mux.is_reconciled(&conn.alias) {
                parts.push((conn.alias.clone(), None));
                continue;
            }
            let actor = if me.is_human() {
                format!("human:{}@{}", conn.owner(), conn.host())
            } else {
                format!("mailbox:{}@{}", p.mailbox_id.unwrap_or_default(), conn.host())
            };
            let mut body = req.body.clone();
            if let Some(obj) = body.as_object_mut() {
                obj.remove("after");
            }
            let shared = Request { body, board: None, from: None, ..req.clone() };
            let conn = conn.clone();
            calls.spawn(async move {
                let alias = conn.alias.clone();
                if !read_all {
                    let resp = forward(&conn, &shared, Some(actor), wait).await;
                    return (alias, resp.ok.then_some(resp.result).flatten());
                }
                // A read changes the board: list first, so a read applied after the cut is
                // never lost unseen; a board whose listing does not answer is left untouched.
                let left = || deadline.saturating_duration_since(tokio::time::Instant::now()).saturating_sub(Duration::from_millis(100)).max(Duration::from_millis(10));
                let list = Request { op: "inbox.list".into(), body: json!({}), ..shared.clone() };
                let listed = forward(&conn, &list, Some(actor.clone()), left()).await;
                let Some(listed) = listed.ok.then_some(listed.result).flatten() else { return (alias, None) };
                let items = listed["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    return (alias, Some(json!({"messages": [], "more": false})));
                }
                // Read exactly the listed messages, each by id: one that arrives meanwhile
                // stays unread, and an answer that does not come in time leaves its message
                // shown unconfirmed with the command that reads it again.
                let mut reads = JoinSet::new();
                for (i, item) in items.iter().enumerate() {
                    let qualified = item["message_id"].as_str().unwrap_or_default().to_string();
                    let one = Request {
                        body: json!({"id": resolve::split_ref(&qualified).1}),
                        idempotency_key: Some(crate::util::new_ulid()),
                        ..shared.clone()
                    };
                    let (conn, actor, wait) = (conn.clone(), actor.clone(), left());
                    reads.spawn(async move {
                        let resp = forward(&conn, &one, Some(actor), wait).await;
                        (i, resp.ok.then_some(resp.result).flatten())
                    });
                }
                let mut answered: Vec<Option<Value>> = vec![None; items.len()];
                while let Some(Ok((i, result))) = reads.join_next().await {
                    answered[i] = result.and_then(|r| r["messages"].as_array().and_then(|m| m.first().cloned()));
                }
                let mut unconfirmed = false;
                let rows: Vec<Value> = items
                    .iter()
                    .zip(answered)
                    .map(|(i, read)| {
                        read.unwrap_or_else(|| {
                            unconfirmed = true;
                            let id = i["message_id"].as_str().unwrap_or_default();
                            json!({"message_id": id, "from": i["from"], "date": i["date"], "place": i["kind"],
                                   "text": i["preview"], "board": alias, "unconfirmed": true,
                                   "recover": format!("conde read {id}")})
                        })
                    })
                    .collect();
                let more = listed["next_cursor"].as_i64().is_some();
                (alias, Some(json!({"messages": rows, "more": more, "unconfirmed": unconfirmed})))
            });
        }
        while !calls.is_empty() {
            match tokio::time::timeout_at(deadline, calls.join_next()).await {
                Ok(Some(Ok(part))) => parts.push(part),
                Ok(Some(Err(_))) => {}
                Ok(None) => break,
                Err(_) => break,
            }
        }
        calls.abort_all();
        // A board that did not answer in time is unavailable.
        for conn in self.boards() {
            let named = parts.iter().any(|(a, _)| *a == conn.alias);
            let published = board::publication_of(self, &conn.alias, &me.id).is_some_and(|p| p.state == PublicationState::Published);
            if published && !named {
                parts.push((conn.alias.clone(), None));
            }
        }
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        parts
    }

    /// `inbox`, `sent`, `read` with no target: the local part and each board's, merged by
    /// time, every row labeled with its board.
    async fn aggregate(self: &Arc<Self>, req: Request) -> Response {
        let id = req.request_id.clone();
        let (local, parts) = tokio::join!(self.local(req.clone()), self.shared_parts(&req));
        let Some(mut result) = local.result.clone().filter(|_| local.ok) else { return local };
        let key = match req.op.as_str() {
            "inbox.list" => "items",
            "receipt.list" => "receipts",
            _ => "messages",
        };
        let mut rows: Vec<Value> = result[key].as_array().cloned().unwrap_or_default();
        for r in rows.iter_mut() {
            label(r, LOCAL_BOARD);
        }
        // Each part carries its own continuation: `next_cursor` (paged with --board <alias>
        // --after N) and `more` (read again).
        let mut local_part = json!(BoardPart { board: LOCAL_BOARD.into(), available: true });
        if let Some(next) = result.get("next_cursor").filter(|v| !v.is_null()) {
            local_part["next_cursor"] = next.clone();
        }
        if result.get("more").and_then(Value::as_bool) == Some(true) {
            local_part["more"] = json!(true);
        }
        let mut board_parts = vec![local_part];
        for (alias, part) in parts {
            let available = part.is_some();
            let mut entry = json!(BoardPart { board: alias.clone(), available });
            if let Some(part) = part {
                rows.extend(part[key].as_array().cloned().unwrap_or_default());
                if let Some(next) = part.get("next_cursor").filter(|v| !v.is_null()) {
                    entry["next_cursor"] = next.clone();
                }
                if part.get("more").and_then(Value::as_bool) == Some(true) {
                    result["more"] = json!(true);
                    entry["more"] = json!(true);
                }
                if part.get("unconfirmed").and_then(Value::as_bool) == Some(true) {
                    entry["unconfirmed"] = json!(true);
                }
            }
            board_parts.push(entry);
        }
        rows.sort_by_key(order_key);
        result[key] = Value::Array(rows);
        result["boards"] = Value::Array(board_parts);
        Response::ok(&id, result)
    }

    /// `whoami`: the local name and address, the caller's publications and the shared boards,
    /// and for an agent its human on each connected board it is published on (`human.status`,
    /// within the aggregate wait).
    async fn whoami(&self, req: Request) -> Response {
        let id = req.request_id.clone();
        let local = self.local(req.clone()).await;
        let Some(mut result) = local.result.clone().filter(|_| local.ok) else { return local };
        let publications = self.local_identity(&req).map(|me| self.publications_of(&me.id)).unwrap_or_default();
        result["publications"] = Value::Array(publications);
        result["boards"] = json!(enrolled_names(self));
        result["humans"] = Value::Array(self.humans(&req).await);
        label(&mut result, LOCAL_BOARD);
        Response::ok(&id, result)
    }

    /// The caller's human on each connected board it is published on, by board.
    async fn humans(&self, req: &Request) -> Vec<Value> {
        let Ok(me) = self.local_identity(req) else { return Vec::new() };
        if me.is_human() {
            return Vec::new();
        }
        let wait = self.mux.tuning.aggregate_wait;
        let mut calls = JoinSet::new();
        for conn in self.boards() {
            let published = board::publication_of(self, &conn.alias, &me.id).filter(|p| p.state == PublicationState::Published);
            let Some(p) = published else { continue };
            if !conn.connected() || !self.mux.is_reconciled(&conn.alias) {
                continue;
            }
            let actor = format!("mailbox:{}@{}", p.mailbox_id.unwrap_or_default(), conn.host());
            let status = Request { op: "human.status".into(), body: json!({}), board: None, from: None, ..req.clone() };
            calls.spawn(async move { forward(&conn, &status, Some(actor), wait).await });
        }
        let mut out = Vec::new();
        while let Ok(Some(Ok(resp))) = tokio::time::timeout(wait, calls.join_next()).await {
            if let Some(r) = resp.result.filter(|_| resp.ok) {
                out.push(r);
            }
        }
        out.sort_by(|a, b| a["board"].as_str().cmp(&b["board"].as_str()));
        out
    }

    /// One agent's publications: `[{board, name: owner/shared, state}]`.
    fn publications_of(&self, agent_id: &str) -> Vec<Value> {
        let pubs = self.local_tx(|tx| Ok(tx.publications(None)?)).unwrap_or_default();
        pubs.into_iter()
            .filter(|p| p.agent_id == agent_id)
            .map(|p| {
                let owner = self.board(&p.board).map(|c| c.owner()).unwrap_or_default();
                json!({"board": p.board, "name": format!("{owner}/{}", p.shared_name), "state": p.state.as_str()})
            })
            .collect()
    }

    pub(crate) fn enrolled(&self) -> Vec<String> {
        self.boards().iter().map(|c| c.alias.clone()).collect()
    }

    /// `doctor`: the local board's checks and this host's live checks: each board's
    /// connection, publications and pending ops, the slots used per origin, the credential
    /// files, recent delivery failures (with details: they stay on this machine).
    async fn doctor(&self, req: Request, target: Target) -> Response {
        let id = req.request_id.clone();
        let actor = req.actor.clone();
        let local = self.local(Request { board: None, ..req }).await;
        let mut lines: Vec<Value> = match (&local.result, &local.error) {
            (Some(r), _) => r["checks"].as_array().cloned().unwrap_or_default(),
            (None, Some(e)) => vec![json!({"ok": false, "text": format!("local board: {}", e.message)})],
            _ => Vec::new(),
        };
        let mut add = |ok: bool, text: String| lines.push(json!({"ok": ok, "text": text}));
        add(true, format!("local board: {}", self.db_path.display()));
        let only = match &target {
            Target::Board(a) => Some(a.clone()),
            Target::Local => Some(LOCAL_BOARD.to_string()),
            Target::Aggregate => None,
        };
        let pubs = self.local_tx(|tx| Ok(tx.publications(None)?)).unwrap_or_default();
        let ops = self.local_tx(|tx| Ok(tx.pending_ops(None)?)).unwrap_or_default();
        for conn in self.boards() {
            if only.as_deref().is_some_and(|o| o != conn.alias) {
                continue;
            }
            let alias = &conn.alias;
            let state = self.board_state(&conn);
            let ok = state == "connected";
            match (&conn.config, conn.client()) {
                (Some(cfg), Some(client)) => add(
                    ok,
                    format!(
                        "{alias}: {state} to {} as host {} (owner {}), instance {}, {} connection(s) since start",
                        cfg.server,
                        cfg.host_id,
                        cfg.owner,
                        client.bridge_instance(),
                        client.connects()
                    ),
                ),
                (Some(cfg), None) => add(false, format!("{alias}: {state} ({} as host {})", cfg.server, cfg.host_id)),
                (None, _) => add(false, format!("{alias}: {state}")),
            }
            let mut names = Vec::new();
            for p in pubs.iter().filter(|p| p.board == *alias) {
                let agent = self.local.with_conn(|tx| tx.agent_by_id(&p.agent_id)).ok().flatten();
                let availability = match (&p.state, agent.as_ref().and_then(|a| a.address.clone())) {
                    (PublicationState::Published, Some(a)) if self.live(&a) => "available".to_string(),
                    (PublicationState::Published, Some(_)) => "no live session".to_string(),
                    (PublicationState::Published, None) => "no session".to_string(),
                    (other, _) => other.as_str().to_string(),
                };
                let local_name = agent.map(|a| a.name).unwrap_or_default();
                let shown = if local_name == p.shared_name { local_name } else { format!("{local_name} as {}", p.shared_name) };
                names.push(format!("{shown} ({availability})"));
            }
            let pending: Vec<String> = ops
                .iter()
                .filter(|o| o.board == *alias)
                .map(|o| format!("{} {}", o.kind.as_str(), o.op_id))
                .collect();
            add(
                pending.is_empty(),
                format!(
                    "{alias} publications: {}; pending ops: {}",
                    if names.is_empty() { "none".to_string() } else { names.join(", ") },
                    if pending.is_empty() { "none".to_string() } else { pending.join(", ") }
                ),
            );
            for removed in conn.removed.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                add(false, format!("{alias}: publication removed: {removed}"));
            }
            if let Some(note) = conn.note.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                add(false, format!("{alias}: {note}"));
            }
            let path = boards::path_in(&self.config_dir, alias);
            let (ok, text) = crate::private::doctor(&path);
            add(ok, text);
        }
        if let Some(legacy) = self.legacy.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            add(false, format!("credentials: {legacy}"));
        }
        for s in self.mux.stats() {
            if only.as_deref().is_some_and(|o| o != s.origin) {
                continue;
            }
            let book = self.board(&s.origin).and_then(|c| c.client()).map(|c| format!(", book {}", c.unacked_outcomes())).unwrap_or_default();
            add(
                s.used < s.budget,
                format!(
                    "slots: {} {}/{} used (queue {}/{}, outcomes {}{book}){}",
                    s.origin,
                    s.used,
                    s.budget,
                    s.queued,
                    s.cap,
                    s.outcomes,
                    s.stopped.map(|w| format!(", {w}")).unwrap_or_default()
                ),
            );
        }
        for f in self.mux.failures() {
            add(true, format!("recent delivery failure: {f}"));
        }
        for (ok, text) in doctor_checks(&HarnessNotifier::new(self.claude_config.clone()), actor.as_deref()) {
            add(ok, text);
        }
        let issues = lines.iter().filter(|l| l["ok"] == false).count();
        let origins: Vec<Value> = self
            .mux
            .stats()
            .into_iter()
            .filter(|s| only.as_deref().is_none_or(|o| o == s.origin))
            .map(|s| json!({"origin": s.origin, "slots_used": s.used, "slots": s.budget, "queued": s.queued}))
            .collect();
        Response::ok(&id, json!({"checks": lines, "issues": issues, "origins": origins, "build": crate::VERSION}))
    }
}

fn enrolled_names(core: &Core) -> Vec<String> {
    core.enrolled()
}

/// Forwards a translated request (the caller's `actor`, no `from`, no `board`) to one board
/// and qualifies the ids of its answer with the alias; rows are labeled with their board.
async fn forward(conn: &BoardConn, req: &Request, actor: Option<String>, wait: Duration) -> Response {
    let id = req.request_id.clone();
    let mut frame = json!({"op": req.op, "body": req.body});
    if let Some(k) = &req.idempotency_key {
        frame["idempotency_key"] = json!(k);
    }
    if let Some(a) = actor {
        frame["actor"] = json!(a);
    }
    match conn.call(frame, wait).await {
        Ok(mut resp) => {
            resp.request_id = id;
            if let Some(result) = resp.result.as_mut() {
                qualify(&conn.alias, result);
                for key in ["items", "receipts", "messages", "hits"] {
                    if let Some(rows) = result.get_mut(key).and_then(Value::as_array_mut) {
                        rows.iter_mut().for_each(|r| label(r, &conn.alias));
                    }
                }
                label(result, &conn.alias);
            }
            resp
        }
        Err(why) => Response::err(&id, ErrorCode::Internal, format!("{why}; the local board is unaffected")),
    }
}

/// Whether a publish op is pending on the board (doctor, tests).
pub fn pending_publish(core: &Core, alias: &str) -> bool {
    core.local_tx(|tx| Ok(tx.pending_ops(Some(alias))?))
        .map(|ops| ops.iter().any(|o| o.kind == BoardOpKind::Publish))
        .unwrap_or(false)
}

/// The bound socket and the inode we own, so shutdown never unlinks a successor's
/// (a Windows pipe disappears with its handles; there is nothing to unlink).
pub(crate) struct Socket {
    listener: Option<ipc::Listener>,
    path: PathBuf,
    inode: (u64, u64),
}

impl Socket {
    fn none() -> Socket {
        Socket { listener: None, path: PathBuf::new(), inode: (0, 0) }
    }
}

#[cfg(unix)]
pub(crate) fn bind(path: &Path) -> anyhow::Result<Socket> {
    let listener = server::bind(path)?;
    let m = std::fs::symlink_metadata(path)?;
    Ok(Socket { listener: Some(listener), path: path.to_path_buf(), inode: (m.dev(), m.ino()) })
}

#[cfg(windows)]
pub(crate) fn bind(path: &Path) -> anyhow::Result<Socket> {
    Ok(Socket { listener: Some(ipc::bind(path)?), path: path.to_path_buf(), inode: (0, 0) })
}

#[cfg(unix)]
fn unlink_own(path: &Path, inode: (u64, u64)) {
    if let Ok(m) = std::fs::symlink_metadata(path)
        && (m.dev(), m.ino()) == inode
    {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(windows)]
fn unlink_own(_path: &Path, _inode: (u64, u64)) {}

/// Serve CLI connections until `stop`; then stop accepting, unlink the
/// socket, let in-flight requests finish and close idle connections.
pub(crate) async fn serve(socket: Socket, handler: Arc<dyn Handler>, mut stop: watch::Receiver<bool>) {
    let Some(mut listener) = socket.listener else { return };
    let mut conns = JoinSet::new();
    let conn_stop = stop.clone();
    loop {
        tokio::select! {
            _ = stop.wait_for(|s| *s) => break,
            accepted = ipc::accept(&mut listener) => match accepted {
                Ok((stream, check)) => match check {
                    Ok(()) => {
                        conns.spawn(connection(stream, handler.clone(), conn_stop.clone()));
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
    drop(listener);
    unlink_own(&socket.path, socket.inode);
    if tokio::time::timeout(DRAIN, async { while conns.join_next().await.is_some() {} }).await.is_err() {
        tracing::warn!("requests still running after {DRAIN:?}; closing them");
    }
}

async fn connection<S>(stream: S, handler: Arc<dyn Handler>, mut stop: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
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
                Ok(req) => handler.clone().handle(req).await,
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

/// A daemon running in this process on its own runtime thread (tests): `stop` ends it the
/// way a process exit does.
pub struct Running {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    core: Arc<Core>,
}

impl Running {
    pub fn start<N: Notifier + Send + 'static>(opts: Options, notifier: N) -> anyhow::Result<Running> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new().name("conde-daemon".into()).spawn(move || {
            let runtime = match runtime() {
                Ok(r) => r,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let daemon = match runtime.block_on(async { Daemon::start(&opts, notifier) }) {
                Ok(d) => d,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(daemon.core().clone()));
            runtime.block_on(daemon.serve(async {
                let _ = stop_rx.await;
            }));
            runtime.shutdown_timeout(Duration::from_secs(2));
        })?;
        let core = ready_rx.recv().context("daemon thread ended")??;
        Ok(Running { stop: Some(stop_tx), thread: Some(thread), core })
    }

    pub fn core(&self) -> &Arc<Core> {
        &self.core
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

#[cfg(test)]
impl Core {
    /// A core over a fresh local board in `dir`, with `boards` registered but not connected.
    pub(crate) fn for_tests(dir: &Path, rt: tokio::runtime::Handle, boards: Vec<BoardConn>) -> Arc<Core> {
        let local = Arc::new(Dispatcher::open(&dir.join("conde.db")).unwrap());
        let principal = local.principal("krisz", "homepc", true).unwrap();
        let core = Arc::new(Core {
            local,
            principal,
            mux: Arc::new(Mux::new(Tuning::default())),
            boards: Mutex::default(),
            reloads: Mutex::default(),
            config_dir: dir.join("config"),
            db_path: dir.join("conde.db"),
            claude_config: None,
            rt,
            legacy: Mutex::default(),
        });
        for b in boards {
            core.add_board(Arc::new(b));
        }
        core
    }
}

impl Core {
    /// Counters of every origin (doctor, tests).
    pub fn origin_stats(&self) -> Vec<OriginStats> {
        self.mux.stats()
    }

    /// The board connection's state as doctor words it.
    pub fn board_status(&self, alias: &str) -> Option<String> {
        self.board(alias).map(|c| self.board_state(&c))
    }

    /// The bridge instance of a board's connection.
    pub fn board_instance(&self, alias: &str) -> Option<String> {
        self.board(alias).and_then(|c| c.client()).map(|c| c.bridge_instance().to_string())
    }
}
