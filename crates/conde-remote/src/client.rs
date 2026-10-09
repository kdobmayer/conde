//! Bridge client: one authenticated WebSocket to the server, kept alive with reconnects.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, header};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::protocol::{self, CODE_BAD_HELLO, CODE_FENCED, CODE_HOST_MISMATCH};
use crate::server::MAX_FRAME_BYTES;
use crate::{Endpoint, Hello, Notify, Outcome, PROTO_V1, Routes};

/// An outcome's reason is cut to this many bytes, so a diagnostic can never make it too large.
pub const MAX_REASON_BYTES: usize = 4096;
const TRUNCATED: &str = " [truncated]";

/// How many delivered attempt ids the client remembers to suppress server resends.
const SEEN_CAPACITY: usize = 65_536;

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    /// `wss://host/v1/bridge` (or `ws://` on a trusted local network or in tests).
    pub url: String,
    /// The host's device credential, sent as `Authorization: Bearer`.
    pub token: String,
    pub host_id: String,
    /// Live harness endpoints announced in the first hello; later changes go through
    /// [`BridgeClient::set_endpoints`].
    pub endpoints: Vec<Endpoint>,
    /// Kept for the process lifetime; generated when `None`.
    pub bridge_instance: Option<String>,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub connect_timeout: Duration,
    /// A connection with no inbound frame (server pings included) for this long is dropped.
    pub idle_timeout: Duration,
    pub call_timeout: Duration,
    /// Delay before resubmitting an outcome the server answered with `retry`; doubles per
    /// outcome up to `outcome_resend_max`.
    pub outcome_resend_min: Duration,
    pub outcome_resend_max: Duration,
    /// A submitted outcome with no ack for this long counts as lost and is resubmitted.
    pub outcome_ack_timeout: Duration,
    /// Outcome submissions awaiting their ack on one connection, at most.
    pub max_outcomes_in_flight: usize,
    /// Admission budget for retained (unacknowledged) outcomes; beyond either limit
    /// [`BridgeClient::report`] answers [`CallError::Backpressure`].
    pub max_unacked_outcomes: usize,
    pub max_unacked_bytes: usize,
    pub max_frame_bytes: usize,
    /// The bridge protocol the hello declares: [`PROTO_V1`] (harness endpoints) or
    /// [`crate::PROTO_V2`] (mailbox endpoints, with `window`).
    pub proto: u32,
    /// The admission window a v2 hello declares; `None` on v1.
    pub window: Option<u32>,
}

impl BridgeConfig {
    pub fn new(
        url: impl Into<String>,
        token: impl Into<String>,
        host_id: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            token: token.into(),
            host_id: host_id.into(),
            endpoints: Vec::new(),
            bridge_instance: None,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(90),
            call_timeout: Duration::from_secs(60),
            outcome_resend_min: Duration::from_secs(2),
            outcome_resend_max: Duration::from_secs(30),
            outcome_ack_timeout: Duration::from_secs(60),
            max_outcomes_in_flight: 4,
            max_unacked_outcomes: 10_000,
            max_unacked_bytes: 8 * 1024 * 1024,
            max_frame_bytes: MAX_FRAME_BYTES,
            proto: PROTO_V1,
            window: None,
        }
    }
}

/// What the server sent, in frame order, as the event callback of
/// [`BridgeClient::start_events`] receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// A new connection's hello was sent; every later event belongs to it.
    Connected,
    Notify(Box<Notify>),
    /// This host's full mailbox route set (v2), with its place in the order of frames
    /// received (see [`BridgeClient::call_ordered`]).
    Routes { routes: Routes, seq: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeState {
    Connecting,
    Connected,
    /// Waiting before the next reconnect attempt.
    Backoff,
    Stopped(StopReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// A newer bridge instance took over this host.
    Fenced,
    /// The server rejected the token (unknown or revoked).
    Unauthorized,
    /// The server refused the hello (`host_mismatch`, `bad_hello`).
    Rejected { code: String, message: String },
    /// [`BridgeClient::shutdown`] was called.
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("request must be a JSON object")]
    BadRequest,
    #[error("bridge stopped: {0:?}")]
    Stopped(StopReason),
    #[error("connection lost before the response arrived")]
    Disconnected,
    #[error("no response within the call timeout")]
    Timeout,
    /// The outcome was not admitted: the retained-outcome budget is full. Nothing was kept;
    /// report it again once [`BridgeClient::unacked_outcomes`] has fallen.
    #[error("too many unacknowledged outcomes; report again after the server acknowledges some")]
    Backpressure,
    /// The outcome's frame can never be sent (over the frame limit or the whole retention
    /// budget). Permanent: reporting it again is refused the same way.
    #[error("outcome frame of {bytes} bytes exceeds the {limit}-byte limit")]
    TooLarge { bytes: usize, limit: usize },
    /// An HTTP request ([`rpc`]) did not get a response object.
    #[error("{0}")]
    Transport(String),
}

/// One request object over `POST <rpc_url>` (`https://host/v1/rpc`) with the host
/// credential, outside any bridge connection, so no hello is sent: what a daemon asks a
/// board before it may connect (board info, the migration preview and apply). `v`, `type`
/// and a `request_id` are filled in. A rejected credential is
/// [`CallError::Stopped`]`(`[`StopReason::Unauthorized`]`)`.
pub async fn rpc(rpc_url: &str, token: &str, request: Value, timeout: Duration) -> Result<Value, CallError> {
    let Value::Object(mut obj) = request else {
        return Err(CallError::BadRequest);
    };
    obj.entry("request_id").or_insert_with(|| json!(format!("{:032x}", rand::random::<u128>())));
    obj.insert("v".into(), json!(protocol::VERSION));
    obj.insert("type".into(), json!("request"));
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured((*tls_config()).clone())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| CallError::Transport(format!("http client: {e}")))?;
    let response = client
        .post(rpc_url)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Value::Object(obj).to_string())
        .send()
        .await
        .map_err(|e| CallError::Transport(format!("{rpc_url}: {e}")))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CallError::Stopped(StopReason::Unauthorized));
    }
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| CallError::Transport(format!("{rpc_url}: {e}")))?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Err(CallError::Transport(format!("{rpc_url}: HTTP {status} without a response object"))),
    }
}

/// Cuts a reason to [`MAX_REASON_BYTES`] on a character boundary, marking the cut.
fn bound_reason(reason: &str) -> String {
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_owned();
    }
    let mut end = MAX_REASON_BYTES - TRUNCATED.len();
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &reason[..end])
}

enum Cmd {
    Endpoints,
    Request { request_id: String, frame: String },
}

type Reply = oneshot::Sender<Result<(Value, u64), CallError>>;

#[derive(Default)]
struct Book {
    /// Reported outcomes not yet acknowledged, in report order.
    unacked: Vec<PendingOutcome>,
    /// Sum of `PendingOutcome::bytes` over `unacked`.
    unacked_bytes: usize,
    calls: HashMap<String, Reply>,
}

impl Book {
    fn take_outcome(&mut self, attempt_id: &str, event_seq: u64) {
        if let Some(i) = self
            .unacked
            .iter()
            .position(|p| p.outcome.attempt_id == attempt_id && p.outcome.event_seq == event_seq)
        {
            let p = self.unacked.remove(i);
            self.unacked_bytes -= p.bytes;
        }
    }
}

/// An outcome awaiting its ack, with its submission schedule.
struct PendingOutcome {
    outcome: Outcome,
    /// Serialized size, counted against `max_unacked_bytes`.
    bytes: usize,
    /// Earliest (re)submission on the current connection.
    not_before: tokio::time::Instant,
    /// While a submission awaits its ack: when that submission counts as lost.
    in_flight_until: Option<tokio::time::Instant>,
    /// Delay after the next `retry` ack or lost submission.
    backoff: Duration,
}

struct Inner {
    config: BridgeConfig,
    instance: String,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    state: watch::Sender<BridgeState>,
    shutdown: watch::Sender<bool>,
    book: Mutex<Book>,
    /// Set once, when the client starts stopping; gates callbacks and reports.
    stop: Mutex<Option<StopReason>>,
    /// The current live endpoints: sent whole in each hello, as diffs in between.
    endpoints: Mutex<Vec<Endpoint>>,
    connects: AtomicU64,
    /// Text frames received from the server, ever: the order of routes frames and responses.
    frames: AtomicU64,
    /// event_seq source for every outcome of this instance: strictly increasing and never
    /// reused within the process lifetime, so it is monotonic per attempt without per-attempt
    /// state. Seeded from the clock (microseconds) so a reused `bridge_instance` usually
    /// continues above an earlier run; that holds only if the clock did not move back between
    /// the runs, so uniqueness is guaranteed per process lifetime, not across a clock rollback.
    next_seq: AtomicU64,
    /// Wakes the connection to submit newly reported outcomes.
    outcome_wake: tokio::sync::Notify,
}

impl Inner {
    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn stop_reason(&self) -> Option<StopReason> {
        self.stop.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Records the stop reason (the first one wins) and returns the one in force.
    fn mark_stopping(&self, reason: StopReason) -> StopReason {
        self.stop
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert(reason)
            .clone()
    }
}

/// Handle to the bridge's server connection. Cheap to clone; the connection runs until
/// [`shutdown`](Self::shutdown), a fence, or a rejected credential.
#[derive(Clone)]
pub struct BridgeClient {
    inner: Arc<Inner>,
}

impl BridgeClient {
    /// Starts connecting in the background (needs a tokio runtime). `on_notify` runs on a
    /// dedicated thread, once per attempt, and may block on delivery; it reports the
    /// result through [`report`](Self::report).
    ///
    /// Once the client stops (fenced, unauthorized, rejected, or [`shutdown`](Self::shutdown)),
    /// notifications still queued for the callback are dropped unrun. A callback already
    /// running completes; its `report` is refused, so its outcome never reaches the server
    /// and the attempt stays unsettled there.
    pub fn start<F>(config: BridgeConfig, mut on_notify: F) -> Self
    where
        F: FnMut(Notify) + Send + 'static,
    {
        Self::start_events(config, move |event| {
            if let Inbound::Notify(n) = event {
                on_notify(*n)
            }
        })
    }

    /// Like [`start`](Self::start), but the callback sees every [`Inbound`] event in frame
    /// order: each connection's start, its notifies and its `routes` frames. An attempt is
    /// marked seen before its notify reaches the callback, and the mark survives reconnects.
    pub fn start_events<F>(config: BridgeConfig, on_event: F) -> Self
    where
        F: FnMut(Inbound) + Send + 'static,
    {
        let instance = config
            .bridge_instance
            .clone()
            .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let config_endpoints = config.endpoints.clone();
        let inner = Arc::new(Inner {
            config,
            instance,
            cmd_tx,
            state: watch::channel(BridgeState::Connecting).0,
            shutdown: watch::channel(false).0,
            book: Mutex::new(Book::default()),
            stop: Mutex::new(None),
            endpoints: Mutex::new(config_endpoints),
            connects: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            next_seq: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |d| d.as_micros() as u64),
            ),
            outcome_wake: tokio::sync::Notify::new(),
        });
        let (notify_tx, notify_rx) = std::sync::mpsc::channel::<Inbound>();
        let mut on_event = on_event;
        let gate = inner.clone();
        std::thread::Builder::new()
            .name("conde-bridge-notify".into())
            .spawn(move || {
                for event in notify_rx {
                    if gate.stop_reason().is_some() {
                        break;
                    }
                    on_event(event);
                }
            })
            .expect("spawn notify thread");
        tokio::spawn(supervise(inner.clone(), cmd_rx, notify_tx));
        Self { inner }
    }

    pub fn bridge_instance(&self) -> &str {
        &self.inner.instance
    }

    pub fn state(&self) -> BridgeState {
        self.inner.state.borrow().clone()
    }

    /// Watches state changes (connected, backoff, stopped).
    pub fn subscribe(&self) -> watch::Receiver<BridgeState> {
        self.inner.state.subscribe()
    }

    /// Resolves when the client stops for good.
    pub async fn stopped(&self) -> StopReason {
        let mut rx = self.inner.state.subscribe();
        let state = rx
            .wait_for(|s| matches!(s, BridgeState::Stopped(_)))
            .await
            .map(|s| s.clone());
        match state {
            Ok(BridgeState::Stopped(reason)) => reason,
            _ => StopReason::Shutdown,
        }
    }

    pub fn shutdown(&self) {
        self.inner.mark_stopping(StopReason::Shutdown);
        let _ = self.inner.shutdown.send(true);
    }

    /// How many times a connection to the server was established.
    pub fn connects(&self) -> u64 {
        self.inner.connects.load(Ordering::Relaxed)
    }

    /// Replaces the live endpoint set; a connected session sends the difference to what
    /// it last announced as one `endpoints` frame, a reconnect announces it in its hello.
    pub fn set_endpoints(&self, endpoints: Vec<Endpoint>) {
        let changed = {
            let mut current = self
                .inner
                .endpoints
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let changed = *current != endpoints;
            *current = endpoints;
            changed
        };
        if changed {
            let _ = self.inner.cmd_tx.send(Cmd::Endpoints);
        }
    }

    /// Queues an attempt outcome and returns its event_seq, or refuses it.
    ///
    /// Until the server acknowledges it (a `retry` ack does not count) the outcome is kept
    /// and resubmitted with the same attempt_id and event_seq: after every reconnect, after a
    /// `retry` ack (`outcome_resend_min` doubling to `outcome_resend_max`), and when a
    /// submission's ack does not arrive within `outcome_ack_timeout`. One submission per
    /// outcome is in flight at a time, at most `max_outcomes_in_flight` per connection.
    ///
    /// event_seq is strictly increasing across every outcome of this instance, so later
    /// reports for one attempt always carry a higher value and are never taken for a resend.
    ///
    /// Admission policy: `reason` is cut to [`MAX_REASON_BYTES`] (with a `[truncated]`
    /// marker). An outcome whose serialized frame would exceed `max_frame_bytes` or
    /// `max_unacked_bytes` can never be sent and is refused for good with
    /// [`CallError::TooLarge`]. At most `max_unacked_outcomes` outcomes and
    /// `max_unacked_bytes` are retained; beyond that the outcome is refused with
    /// [`CallError::Backpressure`] and nothing is kept, so the caller holds the receipt and
    /// pauses delivery until acks drain. An admitted outcome is never dropped while the client
    /// runs. Once the client is stopping, reports are refused with [`CallError::Stopped`].
    pub fn report(
        &self,
        attempt_id: &str,
        status: &str,
        reason: Option<&str>,
    ) -> Result<u64, CallError> {
        if let Some(stop) = self.inner.stop_reason() {
            return Err(CallError::Stopped(stop));
        }
        let cfg = &self.inner.config;
        let mut outcome = Outcome {
            attempt_id: attempt_id.to_owned(),
            // The widest event_seq, so the measured frame is an upper bound.
            event_seq: u64::MAX,
            status: status.to_owned(),
            reason: reason.map(bound_reason),
        };
        // The frame as sent: envelope and JSON escaping included.
        let bytes = protocol::frame("outcome", &outcome).len();
        let limit = cfg.max_frame_bytes.min(cfg.max_unacked_bytes);
        if bytes > limit {
            return Err(CallError::TooLarge { bytes, limit });
        }
        let event_seq = {
            let mut book = self.inner.book();
            if book.unacked.len() >= cfg.max_unacked_outcomes
                || book.unacked_bytes + bytes > cfg.max_unacked_bytes
            {
                return Err(CallError::Backpressure);
            }
            outcome.event_seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
            let event_seq = outcome.event_seq;
            book.unacked_bytes += bytes;
            book.unacked.push(PendingOutcome {
                outcome,
                bytes,
                not_before: tokio::time::Instant::now(),
                in_flight_until: None,
                backoff: cfg.outcome_resend_min,
            });
            event_seq
        };
        self.inner.outcome_wake.notify_one();
        Ok(event_seq)
    }

    /// Outcomes reported but not yet acknowledged by the server.
    pub fn unacked_outcomes(&self) -> usize {
        self.inner.book().unacked.len()
    }

    /// Forwards one request object (DESIGN.md §4) to the server and waits for its response.
    /// `v` and `type` are filled in; the response carries the caller's `request_id` (or the
    /// generated one when the request had none). A request queued while
    /// disconnected is sent after the next connect.
    pub async fn call(&self, request: Value) -> Result<Value, CallError> {
        self.call_ordered(request).await.map(|(v, _)| v)
    }

    /// Like [`call`](Self::call), with the response's place in the order of frames received:
    /// a `routes` frame ([`Inbound::Routes`]) with a lower `seq` was received before this
    /// response, whatever order the two reach their handlers in.
    pub async fn call_ordered(&self, request: Value) -> Result<(Value, u64), CallError> {
        let Value::Object(mut obj) = request else {
            return Err(CallError::BadRequest);
        };
        if let BridgeState::Stopped(reason) = self.state() {
            return Err(CallError::Stopped(reason));
        }
        // Every call gets its own correlation id on the wire, so callers that reuse a
        // request_id never receive each other's response; the caller's id is restored below.
        let request_id = format!("{:032x}", rand::random::<u128>());
        let caller_id = obj.insert("request_id".into(), json!(request_id));
        obj.insert("v".into(), json!(protocol::VERSION));
        obj.insert("type".into(), json!("request"));
        let (tx, rx) = oneshot::channel();
        self.inner.book().calls.insert(request_id.clone(), tx);
        let frame = Value::Object(obj).to_string();
        if self
            .inner
            .cmd_tx
            .send(Cmd::Request {
                request_id: request_id.clone(),
                frame,
            })
            .is_err()
        {
            self.inner.book().calls.remove(&request_id);
            return Err(CallError::Stopped(StopReason::Shutdown));
        }
        match tokio::time::timeout(self.inner.config.call_timeout, rx).await {
            Ok(Ok(Ok((mut response, seq)))) => {
                if let (Some(id), Some(obj)) = (caller_id, response.as_object_mut()) {
                    obj.insert("request_id".into(), id);
                }
                Ok((response, seq))
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(CallError::Disconnected),
            Err(_) => {
                self.inner.book().calls.remove(&request_id);
                Err(CallError::Timeout)
            }
        }
    }
}

enum ConnectError {
    Unauthorized,
    Other(String),
}

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(inner: &Inner) -> Result<Ws, ConnectError> {
    let cfg = &inner.config;
    let mut request = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| ConnectError::Other(format!("bad url: {e}")))?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", cfg.token))
        .map_err(|_| ConnectError::Other("token is not a valid header value".into()))?;
    request.headers_mut().insert(header::AUTHORIZATION, auth);
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(cfg.max_frame_bytes))
        .max_frame_size(Some(cfg.max_frame_bytes));
    let connector = if cfg.url.starts_with("wss://") {
        Connector::Rustls(tls_config())
    } else {
        Connector::Plain
    };
    let attempt = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(ws_config),
        true,
        Some(connector),
    );
    match tokio::time::timeout(cfg.connect_timeout, attempt).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(resp))) if resp.status() == StatusCode::UNAUTHORIZED => {
            Err(ConnectError::Unauthorized)
        }
        Ok(Err(e)) => Err(ConnectError::Other(e.to_string())),
        Err(_) => Err(ConnectError::Other("connect timed out".into())),
    }
}

/// rustls with the ring provider and the Mozilla root set, independent of any process-wide
/// default provider.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

/// Delivered attempt ids, bounded, so a resend after reconnect is not delivered twice.
#[derive(Default)]
struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back(id.to_owned());
        if self.order.len() > SEEN_CAPACITY
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}

enum SessionEnd {
    Lost(String),
    Stop(StopReason),
}

async fn supervise(
    inner: Arc<Inner>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    notify_tx: std::sync::mpsc::Sender<Inbound>,
) {
    let cfg = inner.config.clone();
    let mut shutdown = inner.shutdown.subscribe();
    let mut seen = Seen::default();
    let mut backoff = cfg.backoff_min;
    let reason = loop {
        inner.state.send_replace(BridgeState::Connecting);
        let connected = tokio::select! {
            r = connect(&inner) => r,
            _ = protocol::shutdown_signal(&mut shutdown) => break StopReason::Shutdown,
        };
        match connected {
            Ok(ws) => {
                backoff = cfg.backoff_min;
                inner.connects.fetch_add(1, Ordering::Relaxed);
                inner.state.send_replace(BridgeState::Connected);
                tracing::info!(url = %cfg.url, instance = %inner.instance, "bridge connected");
                match session(
                    &inner,
                    ws,
                    &mut cmd_rx,
                    &notify_tx,
                    &mut seen,
                    &mut shutdown,
                )
                .await
                {
                    SessionEnd::Stop(reason) => break reason,
                    SessionEnd::Lost(why) => tracing::warn!(%why, "bridge connection lost"),
                }
            }
            Err(ConnectError::Unauthorized) => break StopReason::Unauthorized,
            Err(ConnectError::Other(e)) => tracing::warn!(error = %e, "bridge connect failed"),
        }
        inner.state.send_replace(BridgeState::Backoff);
        // Equal jitter: half fixed, half random.
        let half = backoff / 2;
        let delay = half + half.mul_f64(rand::random::<f64>());
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = protocol::shutdown_signal(&mut shutdown) => break StopReason::Shutdown,
        }
        backoff = (backoff * 2).min(cfg.backoff_max);
    };
    let reason = inner.mark_stopping(reason);
    tracing::info!(?reason, instance = %inner.instance, "bridge stopped");
    let calls: Vec<Reply> = inner.book().calls.drain().map(|(_, r)| r).collect();
    for reply in calls {
        let _ = reply.send(Err(CallError::Stopped(reason.clone())));
    }
    inner.state.send_replace(BridgeState::Stopped(reason));
}

async fn session(
    inner: &Inner,
    ws: Ws,
    cmd_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    notify_tx: &std::sync::mpsc::Sender<Inbound>,
    seen: &mut Seen,
    shutdown: &mut watch::Receiver<bool>,
) -> SessionEnd {
    let (mut tx, mut rx) = ws.split();
    let mut inflight: HashSet<String> = HashSet::new();
    let end = run_session(
        inner,
        &mut tx,
        &mut rx,
        cmd_rx,
        notify_tx,
        seen,
        shutdown,
        &mut inflight,
    )
    .await;
    let mut book = inner.book();
    for id in inflight {
        if let Some(reply) = book.calls.remove(&id) {
            let _ = reply.send(Err(CallError::Disconnected));
        }
    }
    end
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    inner: &Inner,
    tx: &mut futures_util::stream::SplitSink<Ws, Message>,
    rx: &mut futures_util::stream::SplitStream<Ws>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    notify_tx: &std::sync::mpsc::Sender<Inbound>,
    seen: &mut Seen,
    shutdown: &mut watch::Receiver<bool>,
    inflight: &mut HashSet<String>,
) -> SessionEnd {
    let cfg = &inner.config;
    let lost = |e: tungstenite::Error| SessionEnd::Lost(e.to_string());
    let mut announced: HashSet<Endpoint> = inner
        .endpoints
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .cloned()
        .collect();
    let hello = protocol::frame(
        "hello",
        &Hello {
            host_id: cfg.host_id.clone(),
            bridge_instance: inner.instance.clone(),
            endpoints: announced.iter().cloned().collect(),
            proto: cfg.proto,
            window: cfg.window,
        },
    );
    if let Err(e) = tx.send(Message::text(hello)).await {
        return lost(e);
    }
    if notify_tx.send(Inbound::Connected).is_err() {
        tracing::error!("notify handler thread is gone");
    }
    // Nothing is in flight on a new connection: every retained outcome is due again.
    {
        let now = tokio::time::Instant::now();
        let mut book = inner.book();
        for p in book.unacked.iter_mut() {
            p.in_flight_until = None;
            p.not_before = now;
        }
    }

    let idle = tokio::time::sleep(cfg.idle_timeout);
    tokio::pin!(idle);
    loop {
        let (frames, wake) = due_outcomes(inner, tokio::time::Instant::now());
        for frame in frames {
            if let Err(e) = tx.send(Message::text(frame)).await {
                return lost(e);
            }
        }
        tokio::select! {
            biased;
            _ = protocol::shutdown_signal(shutdown) => {
                let _ = tx.send(Message::Close(None)).await;
                return SessionEnd::Stop(StopReason::Shutdown);
            }
            _ = &mut idle => return SessionEnd::Lost("no frame from the server within the idle timeout".into()),
            cmd = cmd_rx.recv() => {
                let frame = match cmd {
                    None => return SessionEnd::Stop(StopReason::Shutdown),
                    Some(Cmd::Endpoints) => {
                        let current: HashSet<Endpoint> = inner.endpoints.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
                        let added: Vec<&Endpoint> = current.difference(&announced).collect();
                        let removed: Vec<&Endpoint> = announced.difference(&current).collect();
                        if added.is_empty() && removed.is_empty() { continue }
                        let frame = protocol::frame("endpoints", &json!({"added": added, "removed": removed}));
                        announced = current;
                        frame
                    }
                    Some(Cmd::Request { request_id, frame }) => {
                        // Skip calls that already timed out while queued.
                        if !inner.book().calls.contains_key(&request_id) { continue }
                        inflight.insert(request_id);
                        frame
                    }
                };
                if let Err(e) = tx.send(Message::text(frame)).await { return lost(e); }
            }
            msg = rx.next() => {
                idle.as_mut().reset(tokio::time::Instant::now() + cfg.idle_timeout);
                let text = match msg {
                    None => return SessionEnd::Lost("server closed the stream".into()),
                    Some(Err(e)) => return lost(e),
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(frame))) => {
                        return SessionEnd::Lost(format!("server closed: {frame:?}"));
                    }
                    // tungstenite answers pings itself while reading.
                    Some(Ok(_)) => continue,
                };
                let seq = inner.frames.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(end) = handle_frame(inner, &text, seq, notify_tx, seen, inflight) {
                    return end;
                }
            }
            // A new report, or an ack that freed a submission slot (handled above), reruns
            // the scheduler at the top of the loop.
            _ = inner.outcome_wake.notified() => {}
            _ = tokio::time::sleep_until(wake.unwrap_or_else(tokio::time::Instant::now)), if wake.is_some() => {}
        }
    }
}

/// The outcome scheduler: marks lost submissions, picks the due outcomes to submit within
/// the in-flight cap, and returns their frames with the next instant it has work.
fn due_outcomes(
    inner: &Inner,
    now: tokio::time::Instant,
) -> (Vec<String>, Option<tokio::time::Instant>) {
    let cfg = &inner.config;
    let mut book = inner.book();
    let mut in_flight = 0;
    for p in book.unacked.iter_mut() {
        match p.in_flight_until {
            Some(deadline) if deadline <= now => {
                tracing::warn!(attempt = %p.outcome.attempt_id, seq = p.outcome.event_seq, "outcome ack overdue, resubmitting");
                p.in_flight_until = None;
                p.not_before = now;
                p.backoff = (p.backoff * 2).min(cfg.outcome_resend_max);
            }
            Some(_) => in_flight += 1,
            None => {}
        }
    }
    let mut frames = Vec::new();
    let mut wake: Option<tokio::time::Instant> = None;
    let mut wake_at = |t: tokio::time::Instant| wake = Some(wake.map_or(t, |w| w.min(t)));
    for p in book.unacked.iter_mut() {
        if let Some(deadline) = p.in_flight_until {
            wake_at(deadline);
        } else if p.not_before > now {
            wake_at(p.not_before);
        } else if in_flight < cfg.max_outcomes_in_flight {
            frames.push(protocol::frame("outcome", &p.outcome));
            let deadline = now + cfg.outcome_ack_timeout;
            p.in_flight_until = Some(deadline);
            wake_at(deadline);
            in_flight += 1;
        }
        // Due but over the cap: an ack frees a slot and reruns the scheduler.
    }
    (frames, wake)
}

/// Handles one server frame; returns `Some` when the session must end.
fn handle_frame(
    inner: &Inner,
    text: &str,
    seq: u64,
    notify_tx: &std::sync::mpsc::Sender<Inbound>,
    seen: &mut Seen,
    inflight: &mut HashSet<String>,
) -> Option<SessionEnd> {
    let (kind, value) = match protocol::parse(text) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(error = %e, "bad frame from server");
            return None;
        }
    };
    match kind.as_str() {
        "notify" => match serde_json::from_value::<Notify>(value) {
            Ok(n) => {
                if seen.insert(&n.attempt_id) && notify_tx.send(Inbound::Notify(Box::new(n))).is_err() {
                    tracing::error!("notify handler thread is gone");
                }
            }
            Err(e) => tracing::warn!(error = %e, "bad notify frame"),
        },
        "routes" => match serde_json::from_value::<Routes>(value) {
            Ok(r) => {
                if notify_tx.send(Inbound::Routes { routes: r, seq }).is_err() {
                    tracing::error!("notify handler thread is gone");
                }
            }
            Err(e) => tracing::warn!(error = %e, "bad routes frame"),
        },
        "outcome_ack" => {
            let attempt = value
                .get("attempt_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let seq = value
                .get("event_seq")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let reason = value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut book = inner.book();
            if value.get("retry").and_then(Value::as_bool) == Some(true) {
                // Not recorded: keep it and resubmit after its backoff.
                tracing::debug!(attempt, seq, %reason, "server asked for an outcome resend");
                let max = inner.config.outcome_resend_max;
                if let Some(p) = book
                    .unacked
                    .iter_mut()
                    .find(|p| p.outcome.attempt_id == attempt && p.outcome.event_seq == seq)
                {
                    p.in_flight_until = None;
                    p.not_before = tokio::time::Instant::now() + p.backoff;
                    p.backoff = (p.backoff * 2).min(max);
                }
                return None;
            }
            if value.get("rejected").and_then(Value::as_bool) == Some(true) {
                tracing::warn!(attempt, seq, %reason, "server rejected an outcome");
            }
            book.take_outcome(attempt, seq);
        }
        "response" => {
            let id = value
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            inflight.remove(&id);
            if let Some(reply) = inner.book().calls.remove(&id) {
                let _ = reply.send(Ok((value, seq)));
            }
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            tracing::warn!(%code, %message, "server error frame");
            match code.as_str() {
                CODE_FENCED => return Some(SessionEnd::Stop(StopReason::Fenced)),
                CODE_HOST_MISMATCH | CODE_BAD_HELLO => {
                    return Some(SessionEnd::Stop(StopReason::Rejected { code, message }));
                }
                _ => {}
            }
        }
        other => tracing::debug!(kind = other, "ignored server frame"),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_reason_cuts_on_a_char_boundary_with_a_marker() {
        assert_eq!(bound_reason("short"), "short");
        let cut = bound_reason(&"é".repeat(MAX_REASON_BYTES));
        assert!(cut.len() <= MAX_REASON_BYTES, "{}", cut.len());
        assert!(cut.ends_with(TRUNCATED));
        assert!(cut.trim_end_matches(TRUNCATED).chars().all(|c| c == 'é'));
    }
}
