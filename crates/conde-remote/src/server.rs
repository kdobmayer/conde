//! Server side: `POST /v1/rpc`, `GET /v1/bridge` (WebSocket), `GET /healthz`, plain HTTP on
//! a local listener (TLS is terminated by the reverse proxy in front).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{
    CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval_at, timeout};

use crate::protocol::{self, CODE_BAD_HELLO, CODE_FENCED, CODE_HOST_MISMATCH};
use crate::{
    Admission, Authentication, Authenticator, Dispatcher, Endpoint, Hello, NotificationSource, Outcome, PROTO_V2, Polled,
    Principal, Recorded, Routes, SourceEvent,
};

/// Frame and request-body limit (DESIGN.md §4).
pub const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Transport ping period; keeps the connection inside the ingress idle cut (~240 s).
    pub ping_interval: Duration,
    /// A bridge with this many unanswered pings is closed.
    pub max_missed_pongs: u32,
    /// How often a bridge connection re-reads its host's pending notifications.
    pub poll_interval: Duration,
    /// How long a new bridge connection may take to send its hello.
    pub hello_timeout: Duration,
    pub max_frame_bytes: usize,
    /// Public release files (`https://.../dist`, no trailing slash): `/install.sh`,
    /// `/install.ps1` and `/dist/<file>` redirect there; None answers 404.
    pub dist_base_url: Option<String>,
    /// Entra sign-in, the keys page and the admin API, when configured.
    pub web: Option<crate::web::WebSetup>,
    /// The lowest bridge protocol accepted (`CONDE_MIN_PROTO`); an older hello is refused with
    /// `bad_hello`, which a v1 bridge takes as final.
    pub min_proto: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(30),
            max_missed_pongs: 2,
            poll_interval: Duration::from_secs(1),
            hello_timeout: Duration::from_secs(10),
            max_frame_bytes: MAX_FRAME_BYTES,
            dist_base_url: None,
            web: None,
            min_proto: crate::PROTO_V1,
        }
    }
}

struct Shared {
    auth: Arc<dyn Authenticator>,
    dispatcher: Arc<dyn Dispatcher>,
    source: Arc<dyn NotificationSource>,
    config: ServerConfig,
    /// host_id -> the host's one active bridge connection.
    bridges: Mutex<HashMap<String, ActiveBridge>>,
    next_conn: AtomicU64,
    /// Requests for a full credential recheck and whether its worker runs.
    revalidation: Mutex<Revalidation>,
    shutdown: watch::Receiver<bool>,
    /// Board changes for the browsers' event sockets (topic bits).
    board: tokio::sync::broadcast::Sender<u8>,
    /// The web app, when Entra sign-in is configured.
    app: Mutex<Option<crate::web::app::AppState>>,
}

struct ActiveBridge {
    conn_id: u64,
    instance: String,
    control: mpsc::UnboundedSender<Control>,
    /// The connection's bearer token, rechecked when revocation events may have been missed.
    token: Option<String>,
}

enum Control {
    /// A newer bridge instance took over the host.
    Fenced,
    /// The store has pending work for this host: poll now.
    Poll,
    /// The same instance reconnected; this older connection is obsolete.
    Replaced,
    /// The host's key was revoked or its user disabled.
    Revoked,
}

/// Revocation's view of this replica's bridge connections.
struct Registry(std::sync::Weak<Shared>);

impl crate::web::BridgeRegistry for Registry {
    /// Rechecks the host's live connection against the store (the caller has committed the
    /// revocation) and closes it when its own credential no longer authenticates.
    fn disconnect_host(&self, host_id: &str) {
        let (Some(shared), Ok(runtime)) = (self.0.upgrade(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let host = host_id.to_string();
        runtime.spawn(async move {
            if !revalidate_host(&shared, &host).await {
                retry_revalidation(shared);
            }
        });
    }
}

/// Delivers store events to the connections they concern, until the source or the server stops.
async fn route_events(shared: Arc<Shared>, mut events: tokio::sync::broadcast::Receiver<SourceEvent>) {
    let mut shutdown = shared.shutdown.clone();
    loop {
        let event = tokio::select! {
            _ = protocol::shutdown_signal(&mut shutdown) => return,
            e = events.recv() => e,
        };
        let (host, fenced) = match event {
            Ok(SourceEvent::Board(topics)) => {
                let _ = shared.board.send(topics);
                continue;
            }
            Ok(SourceEvent::Pending(host)) => (host, None),
            Ok(SourceEvent::Fenced(instance)) => (None, Some(instance)),
            Ok(SourceEvent::Revoked(host)) => {
                // Possibly about an older connection's key: the live one is checked itself.
                if !revalidate_host(&shared, &host).await {
                    retry_revalidation(shared.clone());
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => (None, None),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        if host.is_none() && fenced.is_none() {
            // Events may have been missed: the browsers refetch everything.
            let _ = shared.board.send(crate::web::app::ALL_TOPICS);
            // Events may have been missed (a lagging receiver, a listener gap): a lost
            // revocation must not leave its connection open.
            if !revalidate(&shared).await {
                retry_revalidation(shared.clone());
            }
        }
        let bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
        for (h, b) in bridges.iter() {
            match &fenced {
                Some(instance) if *instance == b.instance => {
                    let _ = b.control.send(Control::Fenced);
                }
                Some(_) => {}
                None if host.as_ref().is_none_or(|x| x == h) => {
                    let _ = b.control.send(Control::Poll);
                }
                None => {}
            }
        }
    }
}

/// One credential check off the async workers; a failed check task counts as unavailable.
async fn check(shared: &Shared, token: Option<String>) -> Authentication {
    let Some(token) = token else {
        return Authentication::Invalid;
    };
    let auth = shared.auth.clone();
    tokio::task::spawn_blocking(move || auth.check(&token))
        .await
        .unwrap_or_else(|e| Authentication::Unavailable(e.to_string()))
}

/// Closes the host's live bridge when its own credential no longer authenticates. The verdict
/// applies only to the connection that was checked, never to a newer one. False when the
/// store could not answer (nothing is closed then).
async fn revalidate_host(shared: &Arc<Shared>, host: &str) -> bool {
    let live = {
        let bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
        bridges.get(host).map(|b| (b.conn_id, b.token.clone()))
    };
    let Some((conn_id, token)) = live else {
        return true;
    };
    match check(shared, token).await {
        Authentication::Valid(_) => true,
        Authentication::Invalid => {
            let bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(b) = bridges.get(host).filter(|b| b.conn_id == conn_id) {
                let _ = b.control.send(Control::Revoked);
            }
            true
        }
        Authentication::Unavailable(why) => {
            tracing::warn!(%host, %why, "credential recheck postponed: store unavailable");
            false
        }
    }
}

/// Rechecks every live bridge; false when the store could not answer for some.
async fn revalidate(shared: &Arc<Shared>) -> bool {
    let hosts: Vec<String> = {
        let bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
        bridges.keys().cloned().collect()
    };
    let mut complete = true;
    for host in hosts {
        complete &= revalidate_host(shared, &host).await;
    }
    complete
}

/// Pending full rechecks: every request bumps `requested`; one worker serves them.
#[derive(Default)]
struct Revalidation {
    requested: u64,
    running: bool,
}

/// Asks for every live bridge to be rechecked after a pause, retried with backoff until the
/// store answers. One worker per server; a request made while it runs, even during a pass
/// that then succeeds, gets a pass that starts after it.
fn retry_revalidation(shared: Arc<Shared>) {
    {
        let mut r = shared.revalidation.lock().unwrap_or_else(|p| p.into_inner());
        r.requested += 1;
        if r.running {
            return;
        }
        r.running = true;
    }
    tokio::spawn(async move {
        let mut pause = Duration::from_secs(1);
        let mut shutdown = shared.shutdown.clone();
        loop {
            // The generation this pass serves: read before the pass starts.
            let serving = shared
                .revalidation
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .requested;
            tokio::select! {
                _ = protocol::shutdown_signal(&mut shutdown) => break,
                _ = tokio::time::sleep(pause) => {}
            }
            let complete = revalidate(&shared).await;
            {
                let mut r = shared.revalidation.lock().unwrap_or_else(|p| p.into_inner());
                if complete && r.requested == serving {
                    r.running = false;
                    return;
                }
            }
            pause = if complete {
                Duration::from_secs(1)
            } else {
                (pause * 2).min(Duration::from_secs(30))
            };
        }
        shared
            .revalidation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .running = false;
    });
}

/// A running server. Dropping the handle leaves the server running; call [`shutdown`].
///
/// [`shutdown`]: ServerHandle::shutdown
pub struct ServerHandle {
    local_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<std::io::Result<()>>,
    /// Periodic tasks (the MCP sweep): they end on the shutdown signal and are awaited.
    background: Vec<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Closes every bridge connection, stops accepting, and waits for the listener to end.
    pub async fn shutdown(self) -> std::io::Result<()> {
        let _ = self.shutdown.send(true);
        self.wait().await
    }

    /// Waits until the server stops (only on a listener error or after [`Self::shutdown`]),
    /// then for its periodic tasks to end (signalled here too when the listener failed).
    pub async fn wait(self) -> std::io::Result<()> {
        let served = self.task.await.map_err(std::io::Error::other);
        let _ = self.shutdown.send(true);
        for task in self.background {
            let _ = task.await;
        }
        served?
    }
}

/// Runs the server on `listen` with the default configuration until the listener fails.
pub async fn serve(
    listen: SocketAddr,
    auth: Arc<dyn Authenticator>,
    dispatcher: Arc<dyn Dispatcher>,
    source: Arc<dyn NotificationSource>,
) -> std::io::Result<()> {
    start(listen, ServerConfig::default(), auth, dispatcher, source)
        .await?
        .wait()
        .await
}

/// Binds `listen` and starts serving in the background.
pub async fn start(
    listen: SocketAddr,
    config: ServerConfig,
    auth: Arc<dyn Authenticator>,
    dispatcher: Arc<dyn Dispatcher>,
    source: Arc<dyn NotificationSource>,
) -> std::io::Result<ServerHandle> {
    let listener = TcpListener::bind(listen).await?;
    let local_addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let body_limit = config.max_frame_bytes;
    let web = config.web.clone();
    let mut background = Vec::new();
    let shared = Arc::new(Shared {
        auth,
        dispatcher,
        source,
        config,
        bridges: Mutex::new(HashMap::new()),
        next_conn: AtomicU64::new(1),
        revalidation: Mutex::new(Revalidation::default()),
        shutdown: shutdown_rx.clone(),
        board: tokio::sync::broadcast::channel(256).0,
        app: Mutex::new(None),
    });
    if let Some(events) = shared.source.events() {
        tokio::spawn(route_events(shared.clone(), events));
    }
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/install.sh", get(install_sh))
        .route("/install.ps1", get(install_ps1))
        .route("/dist/{file}", get(dist_file))
        .route("/readme", get(readme_html))
        .route("/readme.md", get(readme_md))
        .route("/favicon.ico", get(crate::web::app::favicon))
        .route("/app/{*path}", get(crate::web::app::asset))
        .route("/v1/rpc", post(rpc))
        .route("/v1/bridge", get(bridge))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(shared.clone());
    let app = match web {
        Some(setup) => {
            let (routes, state) = setup.into_parts(Arc::new(Registry(Arc::downgrade(&shared))));
            let app = app.merge(routes);
            match state {
                Some(web) => {
                    background.push(tokio::spawn(crate::web::oauth::sweep(web.clone(), shutdown_rx.clone())));
                    let mcp = crate::web::mcp::McpState { web: web.clone(), dispatcher: shared.dispatcher.clone() };
                    let ui = crate::web::app::AppState {
                        web,
                        dispatcher: shared.dispatcher.clone(),
                        events: shared.board.clone(),
                        shutdown: shutdown_rx.clone(),
                    };
                    *shared.app.lock().unwrap_or_else(|p| p.into_inner()) = Some(ui.clone());
                    tracing::info!(embedded = crate::web::app::embedded(), "web app enabled");
                    app.merge(crate::web::app::routes(ui)).merge(crate::web::mcp::routes(mcp))
                }
                None => app,
            }
        }
        None => app,
    };
    let mut stop = shutdown_rx;
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move { protocol::shutdown_signal(&mut stop).await })
            .await
    });
    tracing::info!(%local_addr, "conde remote server listening");
    Ok(ServerHandle {
        local_addr,
        shutdown: shutdown_tx,
        task,
        background,
    })
}

/// 200 when the store answers, 503 otherwise (the readiness probe).
async fn healthz(State(shared): State<Arc<Shared>>) -> Response {
    let dispatcher = shared.dispatcher.clone();
    match tokio::task::spawn_blocking(move || dispatcher.healthy()).await {
        Ok(true) => (StatusCode::OK, "ok").into_response(),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "store unavailable\n").into_response(),
    }
}

async fn install_sh(State(shared): State<Arc<Shared>>) -> Response {
    dist_redirect(&shared, "install.sh")
}

async fn install_ps1(State(shared): State<Arc<Shared>>) -> Response {
    dist_redirect(&shared, "install.ps1")
}

async fn dist_file(
    State(shared): State<Arc<Shared>>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> Response {
    dist_redirect(&shared, &file)
}

/// 302 into the release container for one plain file name; never cached, so a
/// new release is seen at once.
fn dist_redirect(shared: &Shared, file: &str) -> Response {
    let plain = file
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric())
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    match &shared.config.dist_base_url {
        Some(base) if plain => (
            StatusCode::FOUND,
            [
                (
                    header::LOCATION,
                    format!("{}/{file}", base.trim_end_matches('/')),
                ),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
        )
            .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "not found\n",
        )
            .into_response(),
    }
}

/// The setup runbook, for teammates without repository access.
const README: &str = include_str!("../../../README.md");

async fn readme_md() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        README,
    )
        .into_response()
}

async fn readme_html() -> Response {
    static HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        use pulldown_cmark::{Options, Parser, html};
        let mut out = String::new();
        html::push_html(&mut out, Parser::new_ext(README, Options::ENABLE_TABLES));
        out
    });
    crate::web::document("conde setup", &HTML)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

/// The caller's principal, or the response refusing it (boxed: a `Response` is large): 401 for
/// an invalid token, 503 (retry) when the store could not answer.
async fn authenticate(shared: &Shared, headers: &HeaderMap) -> Result<Principal, Box<Response>> {
    match check(shared, bearer(headers)).await {
        Authentication::Valid(principal) => Ok(principal),
        Authentication::Invalid => Err(Box::new(unauthorized())),
        Authentication::Unavailable(why) => {
            tracing::warn!(%why, "credential check failed: store unavailable");
            Err(Box::new(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, "5")],
                    "credential store unavailable, retry\n",
                )
                    .into_response(),
            ))
        }
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "unauthorized\n",
    )
        .into_response()
}

/// Runs the dispatcher off the async workers and pins the response's request_id to the
/// request's.
async fn dispatch(shared: &Shared, principal: Principal, request: Value) -> Value {
    dispatch_on(shared, principal, None, request).await
}

/// [`dispatch`] for a request carried by `bridge_instance`'s connection.
async fn dispatch_on(shared: &Shared, principal: Principal, bridge_instance: Option<String>, request: Value) -> Value {
    let request_id = request.get("request_id").cloned().unwrap_or(Value::Null);
    let dispatcher = shared.dispatcher.clone();
    let run = move || match &bridge_instance {
        Some(instance) => dispatcher.dispatch_bridge(&principal, instance, request),
        None => dispatcher.dispatch(&principal, request),
    };
    let mut response =
        match tokio::task::spawn_blocking(run).await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!(error = %e, "dispatcher panicked");
                protocol::error_response(None, "internal", "dispatcher failed")
            }
        };
    if let Some(obj) = response.as_object_mut() {
        obj.insert("request_id".into(), request_id);
    }
    response
}

async fn rpc(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if bearer(&headers).is_none() {
        let app = shared.app.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(app) = app {
            return crate::web::app::rpc(&app, headers, body).await;
        }
    }
    let principal = match authenticate(&shared, &headers).await {
        Ok(p) => p,
        Err(refused) => return *refused,
    };
    match serde_json::from_slice::<Value>(&body) {
        Ok(request @ Value::Object(_)) => {
            axum::Json(dispatch(&shared, principal, request).await).into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            axum::Json(protocol::error_response(
                None,
                "bad_request",
                "body must be one JSON request object",
            )),
        )
            .into_response(),
    }
}

async fn bridge(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let principal = match authenticate(&shared, &headers).await {
        Ok(p) => p,
        Err(refused) => return *refused,
    };
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(rejection) => return rejection.into_response(),
    };
    let max = shared.config.max_frame_bytes;
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| run_bridge(shared, principal, headers, socket))
}

/// `{"v":1,"type":"endpoints","added":[..],"removed":[..]}`: sessions that started or
/// stopped on the bridge's host since its last announcement.
#[derive(Deserialize)]
struct EndpointsDiff {
    #[serde(default)]
    added: Vec<Endpoint>,
    #[serde(default)]
    removed: Vec<Endpoint>,
}

type Sink = SplitSink<WebSocket, Message>;

async fn close_with_error(tx: &mut Sink, code: &str, message: &str) {
    let _ = tx
        .send(Message::text(protocol::error_frame(code, message)))
        .await;
    let _ = tx
        .send(Message::Close(Some(CloseFrame {
            code: 1008,
            reason: code.into(),
        })))
        .await;
}

async fn run_bridge(
    shared: Arc<Shared>,
    principal: Principal,
    headers: HeaderMap,
    socket: WebSocket,
) {
    let (mut tx, mut rx) = socket.split();

    let hello = match timeout(shared.config.hello_timeout, rx.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match protocol::parse(&text) {
            Ok((kind, value)) if kind == "hello" => serde_json::from_value::<Hello>(value)
                .map_err(|e| e.to_string())
                .and_then(|h| h.check().map(|()| h)),
            Ok((kind, _)) => Err(format!("expected hello, got {kind}")),
            Err(e) => Err(e),
        },
        Ok(_) => Err("expected a hello text frame".to_owned()),
        Err(_) => Err("no hello before the timeout".to_owned()),
    };
    let hello = match hello {
        Ok(h) => h,
        Err(e) => return close_with_error(&mut tx, CODE_BAD_HELLO, &e).await,
    };
    if hello.host_id != principal.host_id {
        tracing::warn!(claimed = %hello.host_id, token_host = %principal.host_id, "bridge hello host mismatch");
        return close_with_error(
            &mut tx,
            CODE_HOST_MISMATCH,
            "hello host_id does not match the credential's host",
        )
        .await;
    }
    if hello.proto < shared.config.min_proto {
        let why = format!("upgrade conde: this board takes bridge protocol {} or newer", shared.config.min_proto);
        return close_with_error(&mut tx, CODE_BAD_HELLO, &why).await;
    }
    let host = hello.host_id.clone();
    let instance = hello.bridge_instance.clone();
    let (proto, window) = (hello.proto, hello.window);

    let conn_id = shared.next_conn.fetch_add(1, Ordering::Relaxed);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let endpoints: HashSet<Endpoint> = hello.endpoints.iter().filter(|e| speaks(proto, e)).cloned().collect();
    let fence = {
        let (shared, host, instance) = (shared.clone(), host.clone(), instance.clone());
        let endpoints: Vec<Endpoint> = endpoints.iter().cloned().collect();
        let (token, admitted) = (bearer(&headers), principal.clone());
        tokio::task::spawn_blocking(move || {
            // Held across fence() so concurrent hellos for one host serialize.
            let mut bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
            // Revalidated under the registry lock before any side effect: a revocation commits
            // before taking this lock to disconnect, so it either fails this check or finds the
            // registered connection.
            match token.as_deref().map(|t| shared.auth.check(t)) {
                Some(Authentication::Valid(current)) if current == admitted => {}
                Some(Authentication::Unavailable(why)) => return Err(Some(why)),
                _ => return Err(None),
            }
            let result = shared.source.admit(&host, &hello);
            if result == Admission::Active {
                let entry = ActiveBridge {
                    conn_id,
                    instance: instance.clone(),
                    control: control_tx,
                    token,
                };
                if let Some(prev) = bridges.insert(host.clone(), entry) {
                    let kick = if prev.instance == instance {
                        Control::Replaced
                    } else {
                        Control::Fenced
                    };
                    let _ = prev.control.send(kick);
                }
                drop(bridges);
                shared.source.endpoints(&host, &instance, &endpoints);
            }
            Ok(result)
        })
        .await
    };
    match fence {
        Ok(Ok(Admission::Active)) => {}
        Ok(Ok(Admission::Refused { code, message })) => {
            return close_with_error(&mut tx, &code, &message).await;
        }
        Ok(Err(None)) => {
            return close_with_error(&mut tx, "revoked", "this host's key was revoked").await;
        }
        // Not a verdict on the key: the bridge reconnects after its backoff.
        Ok(Err(Some(why))) => {
            tracing::warn!(%host, %why, "hello refused for now: store unavailable");
            return close_with_error(
                &mut tx,
                "unavailable",
                "credential store unavailable, retry",
            )
            .await;
        }
        Ok(Ok(Admission::Fenced)) => {
            return close_with_error(
                &mut tx,
                CODE_FENCED,
                "a newer bridge instance is active for this host",
            )
            .await;
        }
        Err(e) => {
            tracing::error!(error = %e, "fence failed");
            return close_with_error(&mut tx, "internal", "fence failed").await;
        }
    }
    // Defence in depth: the credential is checked once more now that the connection is
    // registered and reachable by a revocation's disconnect.
    if !revalidate_host(&shared, &host).await {
        retry_revalidation(shared.clone());
    }
    tracing::info!(%host, %instance, conn_id, "bridge connected");

    let mut conn = Conn {
        shared: shared.clone(),
        principal,
        host: host.clone(),
        instance: instance.clone(),
        conn_id,
        sent: HashSet::new(),
        endpoints,
        heartbeat_pending: Arc::default(),
        last_heartbeat: None,
        proto,
        window,
        routes: None,
    };
    let reason = conn.run(&mut tx, &mut rx, &mut control_rx).await;
    tracing::info!(%host, %instance, conn_id, reason, "bridge disconnected");

    let mut bridges = shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
    if bridges.get(&host).is_some_and(|b| b.conn_id == conn_id) {
        bridges.remove(&host);
    }
}

/// Whether an endpoint belongs to a bridge protocol: harness endpoints to v1, mailboxes to v2.
fn speaks(proto: u32, e: &Endpoint) -> bool {
    matches!(e, Endpoint::Mailbox { .. }) == (proto == PROTO_V2)
}

/// Outcomes queued per connection before reading pauses; a well-behaved bridge keeps at most
/// its `max_outcomes_in_flight` (4) outstanding.
const OUTCOME_QUEUE: usize = 64;

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Records one connection's outcomes in arrival order and answers each with an ack.
async fn record_outcomes(
    shared: Arc<Shared>,
    host: String,
    instance: String,
    mut outcomes: mpsc::Receiver<Outcome>,
    acks: mpsc::UnboundedSender<String>,
) {
    while let Some(outcome) = outcomes.recv().await {
        let (source, h, i, o) = (
            shared.source.clone(),
            host.clone(),
            instance.clone(),
            outcome.clone(),
        );
        let recorded = tokio::task::spawn_blocking(move || source.record_outcome(&h, &i, &o))
            .await
            .unwrap_or_else(|e| Recorded::Retry(format!("record_outcome failed: {e}")));
        let (attempt, seq) = (&outcome.attempt_id, outcome.event_seq);
        // Every outcome is answered; only `retry` leaves it with the bridge for a resend.
        let (rejected, retry, reason) = match recorded {
            Recorded::Accepted => {
                tracing::debug!(%attempt, seq, "outcome recorded");
                (false, false, None)
            }
            Recorded::Rejected(why) => {
                tracing::warn!(%attempt, %host, %instance, %why, "outcome rejected");
                (true, false, Some(why))
            }
            Recorded::Retry(why) => {
                tracing::warn!(%attempt, seq, %why, "outcome not recorded, bridge will resend");
                (false, true, Some(why))
            }
        };
        let ack = json!({"attempt_id": attempt, "event_seq": seq, "rejected": rejected, "retry": retry, "reason": reason});
        if acks.send(protocol::frame("outcome_ack", &ack)).is_err() {
            break;
        }
    }
}

struct Conn {
    shared: Arc<Shared>,
    principal: Principal,
    host: String,
    instance: String,
    conn_id: u64,
    /// Attempts already pushed on this connection and still pending.
    sent: HashSet<String>,
    /// The bridge's live endpoints: its hello's set, updated by `endpoints` diff frames.
    endpoints: HashSet<Endpoint>,
    /// A heartbeat write is in flight (at most one per connection).
    heartbeat_pending: Arc<std::sync::atomic::AtomicBool>,
    /// When the last heartbeat write was started.
    last_heartbeat: Option<tokio::time::Instant>,
    /// The hello's bridge protocol and (v2) admission window.
    proto: u32,
    window: Option<u32>,
    /// v2: the routes last sent on this connection (None until the first is sent).
    routes: Option<Routes>,
}

impl Conn {
    /// Whether this connection is still its host's active bridge, read under the registry lock.
    /// It gates the admission of each notify write: once a takeover is registered, no further
    /// write is started. The lock is released before the async send, so a write admitted just
    /// before the takeover may still complete after it; the bridge's fence decides from there.
    fn is_active(&self) -> bool {
        let bridges = self
            .shared
            .bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        bridges
            .get(&self.host)
            .is_some_and(|b| b.conn_id == self.conn_id)
    }

    async fn run(
        &mut self,
        tx: &mut Sink,
        rx: &mut futures_util::stream::SplitStream<WebSocket>,
        control: &mut mpsc::UnboundedReceiver<Control>,
    ) -> &'static str {
        let cfg = self.shared.config.clone();
        let mut shutdown = self.shared.shutdown.clone();
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<String>();
        let (outcome_tx, outcome_rx) = mpsc::channel::<Outcome>(OUTCOME_QUEUE);
        let _worker = AbortOnDrop(tokio::spawn(record_outcomes(
            self.shared.clone(),
            self.host.clone(),
            self.instance.clone(),
            outcome_rx,
            resp_tx.clone(),
        )));
        let mut ping = interval_at(
            tokio::time::Instant::now() + cfg.ping_interval,
            cfg.ping_interval,
        );
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut poll = interval_at(
            tokio::time::Instant::now() + cfg.poll_interval,
            cfg.poll_interval,
        );
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut unanswered = 0u32;
        let mut replayed = false;

        loop {
            tokio::select! {
                biased;
                _ = protocol::shutdown_signal(&mut shutdown) => {
                    let _ = tx.send(Message::Close(Some(CloseFrame { code: 1001, reason: "server shutdown".into() }))).await;
                    return "shutdown";
                }
                kick = control.recv() => {
                    match kick {
                        Some(Control::Replaced) | None => {
                            let _ = tx.send(Message::Close(Some(CloseFrame { code: 1000, reason: "replaced".into() }))).await;
                            return "replaced by the same instance";
                        }
                        Some(Control::Fenced) => {
                            close_with_error(tx, CODE_FENCED, "a newer bridge instance took over this host").await;
                            return "fenced";
                        }
                        Some(Control::Revoked) => {
                            close_with_error(tx, "revoked", "this host's key was revoked").await;
                            return "revoked";
                        }
                        Some(Control::Poll) => {
                            if !self.push_pending(tx).await { return "connection lost"; }
                        }
                    }
                }
                // Initial replay: once, after a pending control message, before any bridge frame.
                _ = std::future::ready(()), if !replayed => {
                    replayed = true;
                    if !self.push_pending(tx).await { return "connection lost"; }
                }
                msg = rx.next() => {
                    let text = match msg {
                        None | Some(Err(_)) => return "connection lost",
                        Some(Ok(Message::Close(_))) => return "closed by bridge",
                        Some(Ok(Message::Pong(_))) => {
                            // Only the answer to an outstanding ping proves the bridge alive.
                            if unanswered > 0 {
                                unanswered = 0;
                                self.heartbeat(cfg.ping_interval);
                            }
                            continue
                        }
                        Some(Ok(Message::Ping(_))) => continue,
                        Some(Ok(Message::Binary(_))) => {
                            let _ = tx.send(Message::text(protocol::error_frame("bad_frame", "binary frames are not supported"))).await;
                            continue;
                        }
                        Some(Ok(Message::Text(text))) => text,
                    };
                    if let Some(reply) = self.handle(&text, &resp_tx, &outcome_tx).await
                        && tx.send(Message::text(reply)).await.is_err()
                    {
                        return "connection lost";
                    }
                }
                Some(reply) = resp_rx.recv() => {
                    if tx.send(Message::text(reply)).await.is_err() { return "connection lost"; }
                }
                _ = ping.tick() => {
                    if unanswered >= cfg.max_missed_pongs {
                        let _ = tx.send(Message::Close(Some(CloseFrame { code: 1011, reason: "ping timeout".into() }))).await;
                        return "ping timeout";
                    }
                    if tx.send(Message::Ping(Bytes::new())).await.is_err() { return "connection lost"; }
                    unanswered += 1;
                }
                // Last: a source slower than the poll period must not starve the branches above.
                _ = poll.tick() => {
                    if !self.push_pending(tx).await { return "connection lost"; }
                }
            }
        }
    }

    /// Records that the bridge answered a ping, off the async workers and without waiting: at
    /// most one write in flight per connection, and at most one per half ping period.
    fn heartbeat(&mut self, ping_interval: Duration) {
        use std::sync::atomic::Ordering::SeqCst;
        let now = tokio::time::Instant::now();
        if self.last_heartbeat.is_some_and(|last| now.duration_since(last) < ping_interval / 2)
            || self.heartbeat_pending.swap(true, SeqCst)
        {
            return;
        }
        self.last_heartbeat = Some(now);
        let (source, host, instance) = (self.shared.source.clone(), self.host.clone(), self.instance.clone());
        let pending = self.heartbeat_pending.clone();
        tokio::task::spawn_blocking(move || {
            source.heartbeat(&host, &instance);
            pending.store(false, SeqCst);
        });
    }

    /// Sends pending notifications not yet pushed; false when the connection is lost. A fenced
    /// or replaced connection stops writing and learns it from `control` on the next turn.
    async fn push_pending(&mut self, tx: &mut Sink) -> bool {
        for frame in self.new_notifications().await {
            if !self.is_active() {
                break;
            }
            if tx.send(Message::text(frame)).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Pending notifications not yet pushed on this connection, as notify frames. On v2 the
    /// host's routes go first whenever they differ from the last sent (always on connect), so
    /// the bridge applies a revocation before any further notify.
    async fn new_notifications(&mut self) -> Vec<String> {
        let (source, host, instance, window) = (
            self.shared.source.clone(),
            self.host.clone(),
            self.instance.clone(),
            self.window,
        );
        let polled = tokio::task::spawn_blocking(move || match window {
            Some(w) => source.poll_window(&host, &instance, w),
            None => source.poll(&host, &instance),
        });
        let mut frames = Vec::new();
        let pending = match polled.await {
            Ok(Polled::Due(p)) => p,
            Ok(Polled::Routed { notifies, routes }) => {
                if self.proto == PROTO_V2 && self.routes.as_ref() != Some(&routes) {
                    frames.push(protocol::frame("routes", &routes));
                    self.routes = Some(routes);
                }
                notifies
            }
            Ok(Polled::Fenced) => {
                // Durably fenced (an event may have been missed): close this connection.
                let bridges = self.shared.bridges.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(b) = bridges.get(&self.host).filter(|b| b.conn_id == self.conn_id) {
                    let _ = b.control.send(Control::Fenced);
                }
                return Vec::new();
            }
            Err(e) => {
                tracing::error!(error = %e, "pending_for_host failed");
                return Vec::new();
            }
        };
        let current: HashSet<&str> = pending.iter().map(|n| n.attempt_id.as_str()).collect();
        self.sent.retain(|a| current.contains(a.as_str()));
        for n in &pending {
            if self.sent.insert(n.attempt_id.clone()) {
                frames.push(protocol::frame("notify", n));
            }
        }
        frames
    }

    /// Handles one inbound text frame; returns an immediate reply frame, if any. Requests are
    /// dispatched in the background and answered through `resp_tx`.
    async fn handle(
        &mut self,
        text: &str,
        resp_tx: &mpsc::UnboundedSender<String>,
        outcome_tx: &mpsc::Sender<Outcome>,
    ) -> Option<String> {
        let (kind, value) = match protocol::parse(text) {
            Ok(parsed) => parsed,
            Err(e) => return Some(protocol::error_frame("bad_frame", &e)),
        };
        match kind.as_str() {
            "outcome" => {
                let outcome: Outcome = match serde_json::from_value(value) {
                    Ok(o) => o,
                    Err(e) => {
                        return Some(protocol::error_frame(
                            "bad_frame",
                            &format!("bad outcome: {e}"),
                        ));
                    }
                };
                // A v2 bridge reports the closed statuses and reasons only (DESIGN-v2.md §5).
                if self.proto == PROTO_V2
                    && let Err(why) = outcome.check_v2()
                {
                    tracing::warn!(attempt = %outcome.attempt_id, host = %self.host, %why, "outcome rejected");
                    let ack = json!({"attempt_id": outcome.attempt_id, "event_seq": outcome.event_seq,
                                     "rejected": true, "retry": false, "reason": why});
                    return Some(protocol::frame("outcome_ack", &ack));
                }
                // Recorded in order by the connection's outcome worker, so a slow store
                // never holds up the request frames read after it.
                let _ = outcome_tx.send(outcome).await;
                None
            }
            "endpoints" => {
                let diff: EndpointsDiff = match serde_json::from_value(value) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(protocol::error_frame(
                            "bad_frame",
                            &format!("bad endpoints frame: {e}"),
                        ));
                    }
                };
                for e in &diff.removed {
                    self.endpoints.remove(e);
                }
                let proto = self.proto;
                self.endpoints.extend(diff.added.into_iter().filter(|e| speaks(proto, e)));
                let (source, host, instance) = (
                    self.shared.source.clone(),
                    self.host.clone(),
                    self.instance.clone(),
                );
                let all: Vec<Endpoint> = self.endpoints.iter().cloned().collect();
                if let Err(e) =
                    tokio::task::spawn_blocking(move || source.endpoints(&host, &instance, &all))
                        .await
                {
                    tracing::error!(error = %e, "endpoints update failed");
                }
                None
            }
            "request" => {
                let (shared, principal) = (self.shared.clone(), self.principal.clone());
                let instance = self.instance.clone();
                let resp_tx = resp_tx.clone();
                tokio::spawn(async move {
                    let mut response = dispatch_on(&shared, principal, Some(instance), value).await;
                    if let Some(obj) = response.as_object_mut() {
                        obj.insert("v".into(), json!(protocol::VERSION));
                        obj.insert("type".into(), json!("response"));
                    }
                    let _ = resp_tx.send(response.to_string());
                });
                None
            }
            "hello" => Some(protocol::error_frame(
                "bad_frame",
                "hello was already received on this connection",
            )),
            other => Some(protocol::error_frame(
                "bad_frame",
                &format!("unknown frame type {other}"),
            )),
        }
    }
}

#[cfg(test)]
mod revalidation_tests {
    use super::*;
    use crate::FenceResult;
    use std::sync::atomic::AtomicUsize;

    /// Host "a" checks valid, then "b"'s check blocks until released; meanwhile "a" is
    /// revoked while the store is unavailable (call 2); afterwards "a" is invalid.
    struct ControlledAuth {
        calls: AtomicUsize,
        first: Mutex<Option<String>>,
        second_entered: tokio::sync::Notify,
        release: (Mutex<bool>, std::sync::Condvar),
    }

    impl Authenticator for ControlledAuth {
        fn authenticate(&self, _: &str) -> Option<Principal> {
            unreachable!("the server uses check")
        }
        fn check(&self, token: &str) -> Authentication {
            let valid = || {
                Authentication::Valid(Principal {
                    user_id: "u".into(),
                    host_id: token.into(),
                })
            };
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    *self.first.lock().unwrap() = Some(token.into());
                    valid()
                }
                1 => {
                    self.second_entered.notify_one();
                    let (lock, cv) = &self.release;
                    let mut go = lock.lock().unwrap();
                    while !*go {
                        go = cv.wait(go).unwrap();
                    }
                    valid()
                }
                2 => Authentication::Unavailable("store unavailable after the revocation".into()),
                _ if self.first.lock().unwrap().as_deref() == Some(token) => Authentication::Invalid,
                _ => valid(),
            }
        }
    }

    struct Empty;

    impl Dispatcher for Empty {
        fn dispatch(&self, _: &Principal, _: Value) -> Value {
            json!({})
        }
    }

    impl NotificationSource for Empty {
        fn pending_for_host(&self, _: &str, _: &str) -> Vec<crate::Notify> {
            Vec::new()
        }
        fn record_outcome(&self, _: &str, _: &str, _: &Outcome) -> Recorded {
            Recorded::Accepted
        }
        fn fence(&self, _: &str, _: &str) -> FenceResult {
            FenceResult::Active
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recheck_requested_during_a_successful_pass_still_runs() {
        let auth = Arc::new(ControlledAuth {
            calls: AtomicUsize::new(0),
            first: Mutex::new(None),
            second_entered: tokio::sync::Notify::new(),
            release: (Mutex::new(false), std::sync::Condvar::new()),
        });
        let (stop, shutdown) = watch::channel(false);
        let mut bridges = HashMap::new();
        let mut controls = HashMap::new();
        for (id, host) in ["a", "b"].into_iter().enumerate() {
            let (control, rx) = mpsc::unbounded_channel();
            controls.insert(host.to_string(), rx);
            let bridge = ActiveBridge {
                conn_id: id as u64,
                instance: host.into(),
                control,
                token: Some(host.into()),
            };
            bridges.insert(host.to_string(), bridge);
        }
        let shared = Arc::new(Shared {
            auth: auth.clone(),
            dispatcher: Arc::new(Empty),
            source: Arc::new(Empty),
            config: ServerConfig::default(),
            bridges: Mutex::new(bridges),
            next_conn: AtomicU64::new(3),
            revalidation: Mutex::new(Revalidation::default()),
            shutdown,
            board: tokio::sync::broadcast::channel(1).0,
            app: Mutex::new(None),
        });
        retry_revalidation(shared.clone());
        tokio::time::timeout(Duration::from_secs(3), auth.second_entered.notified())
            .await
            .unwrap();
        let first = auth.first.lock().unwrap().clone().unwrap();
        // The first host, already checked valid in this pass, is revoked while the store is
        // unavailable: its recheck asks for another pass.
        assert!(!revalidate_host(&shared, &first).await);
        retry_revalidation(shared.clone());
        *auth.release.0.lock().unwrap() = true;
        auth.release.1.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while shared.revalidation.lock().unwrap().running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the worker finishes");
        assert!(auth.calls.load(Ordering::SeqCst) >= 4, "the requested pass never ran");
        let revoked = controls.get_mut(&first).unwrap().try_recv();
        assert!(matches!(revoked, Ok(Control::Revoked)), "the revoked bridge stayed open");
        let other = if first == "a" { "b" } else { "a" };
        assert!(controls.get_mut(other).unwrap().try_recv().is_err(), "a valid bridge was closed");
        stop.send(true).unwrap();
    }
}
