//! Cloud mode, server side: the board behind conde-remote's listener. Every
//! piece of state (credentials, fencing, attempts, endpoints) lives in the
//! store and is reached through the dispatcher's ops, so a restarted server
//! loses or duplicates nothing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use conde_remote::{
    Admission, Authentication, Authenticator, Endpoint, FenceResult, Hello, NotificationSource, Notify, NotifyFields, Outcome,
    Polled, Principal as RemotePrincipal, Recorded, Route, Routes, ServerConfig, ServerHandle, SourceEvent, WebPrincipal,
};
use tokio::sync::broadcast;
use serde_json::{Value, json};

use crate::address::{Address, Kind};
use crate::dispatch::{Dispatcher, Principal};
use crate::ops;
use crate::protocol::{ErrorCode, Request, Response};
use crate::store::Wake;
use crate::util::{iso_from_ms, new_ulid};

/// How long SIGTERM may take before the server stops waiting for connections.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(50);

/// How long a bridge op retries a transient storage failure (a held write lock).
pub const TRANSIENT_RETRY: Duration = Duration::from_secs(10);

/// Why a bridge op did not complete.
#[derive(Debug)]
enum OpFailure {
    /// The board refused it (wrong owner, unknown host, bad input): final.
    Refused(String),
    /// Storage did not complete it (locked, I/O): the same call may succeed later.
    Transient(String),
}

/// Ends the current blocking callback without an answer. conde-remote runs
/// each callback on a blocking task and treats an unwound one as "no result":
/// an outcome is then not acknowledged (the bridge keeps it and resends it on
/// its next connection) and a hello is closed with an internal error (the
/// bridge reconnects). `resume_unwind` skips the panic hook: this is control
/// flow, not a crash.
fn unanswered(why: String) -> ! {
    std::panic::resume_unwind(Box::new(why))
}

/// The board as conde-remote sees it: dispatcher, authenticator and outbox.
#[derive(Clone)]
pub struct Board {
    d: Arc<Dispatcher>,
    retry: Duration,
}

impl Board {
    pub fn new(d: Arc<Dispatcher>) -> Board {
        Board { d, retry: TRANSIENT_RETRY }
    }

    /// Change how long transient storage failures are retried (tests).
    pub fn with_transient_retry(mut self, retry: Duration) -> Board {
        self.retry = retry;
        self
    }

    /// Run a bridge op as `host`'s bridge `instance`; storage failures are
    /// retried for `self.retry` (every bridge op is idempotent: claims are
    /// transactional, reports dedup on event_seq, fence and endpoints converge).
    fn bridge_op(&self, host: &str, instance: &str, op: &str, body: Value) -> Result<Value, OpFailure> {
        let deadline = std::time::Instant::now() + self.retry;
        let mut pause = Duration::from_millis(50);
        loop {
            let failure = match self.bridge_op_once(host, instance, op, &body) {
                Err(OpFailure::Transient(why)) => why,
                other => return other,
            };
            if std::time::Instant::now() + pause > deadline {
                return Err(OpFailure::Transient(failure));
            }
            tracing::warn!(host, instance, op, "transient store failure, retrying: {failure}");
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_secs(1));
        }
    }

    /// The principal of `host`'s owner (who must be `user_id` when given),
    /// from one owner read: Ok(None) when the host is not (or no longer) that
    /// user's, Err when the store did not answer.
    fn principal(&self, host: &str, user_id: Option<i64>, instance: Option<&str>) -> Result<Option<Principal>, String> {
        let owner = self.d.with_conn(|c| ops::hosts::host_owner(c, host)).map_err(|e| format!("storage error: {e}"))?;
        Ok(owner.filter(|(id, _)| user_id.is_none_or(|u| u == *id)).map(|(user_id, user)| Principal {
            user_id,
            user,
            host: host.to_string(),
            local: false,
            bridge_instance: instance.map(str::to_string),
            bridge_connection: None,
            browser: false,
        }))
    }

    fn bridge_op_once(&self, host: &str, instance: &str, op: &str, body: &Value) -> Result<Value, OpFailure> {
        let p = match self.principal(host, None, Some(instance)) {
            Ok(Some(p)) => p,
            Ok(None) => return Err(OpFailure::Refused(format!("host '{host}' is not enrolled"))),
            Err(why) => return Err(OpFailure::Transient(why)),
        };
        let resp = self.d.dispatch(&p, &Request::new(new_ulid(), op, body.clone()));
        match (resp.result, resp.error) {
            (Some(r), None) => Ok(r),
            (_, Some(e)) if e.code == ErrorCode::Internal => Err(OpFailure::Transient(e.message)),
            (_, Some(e)) => Err(OpFailure::Refused(e.message)),
            _ => Err(OpFailure::Transient(format!("{op}: empty response"))),
        }
    }
}

impl conde_remote::Dispatcher for Board {
    fn dispatch(&self, principal: &RemotePrincipal, request: Value) -> Value {
        self.dispatch_as(principal, None, request)
    }

    fn dispatch_bridge(&self, principal: &RemotePrincipal, bridge_instance: &str, request: Value) -> Value {
        self.dispatch_as(principal, Some(bridge_instance), request)
    }

    /// A browser read as the signed-in owner: no actor, no host, so only owner-scoped and
    /// public reads answer (conde-remote lets nothing else through).
    fn dispatch_web(&self, user: &WebPrincipal, request: Value) -> Value {
        let id = request.get("request_id").and_then(Value::as_str).unwrap_or_default().to_string();
        let resp = match serde_json::from_value::<Request>(request) {
            Err(e) => Response::err(&id, ErrorCode::BadRequest, format!("bad request frame: {e}")),
            Ok(req) => {
                let p = Principal {
                    user_id: user.user_id,
                    user: user.user.clone(),
                    host: conde_remote::web::BROWSER_HOST.to_string(),
                    local: false,
                    bridge_instance: None,
                    bridge_connection: None,
                    browser: true,
                };
                self.d.dispatch(&p, &Request { actor: None, from: None, idempotency_key: None, ..req })
            }
        };
        serde_json::to_value(resp).unwrap_or_else(|e| json!({"v": 1, "type": "response", "request_id": id, "ok": false,
            "error": {"code": "internal", "message": e.to_string()}}))
    }

    fn healthy(&self) -> bool {
        self.d.healthy()
    }
}

impl Board {
    /// A remote request; one carried by a bridge connection names its instance,
    /// which the dispatcher refuses once fenced.
    fn dispatch_as(&self, principal: &RemotePrincipal, bridge: Option<&str>, request: Value) -> Value {
        let id = request.get("request_id").and_then(Value::as_str).unwrap_or_default().to_string();
        let resp = match serde_json::from_value::<Request>(request) {
            Err(e) => Response::err(&id, ErrorCode::BadRequest, format!("bad request frame: {e}")),
            Ok(req) => match principal.user_id.parse() {
                Err(_) => Response::err(&id, ErrorCode::Unauthenticated, "this host's credential no longer maps to a user"),
                Ok(uid) => match self.principal(&principal.host_id, Some(uid), None) {
                    Ok(Some(p)) => {
                        let p = Principal { bridge_connection: bridge.map(str::to_string), ..p };
                        self.d.dispatch(&p, &req)
                    }
                    Ok(None) => {
                        Response::err(&id, ErrorCode::Unauthenticated, "this host's credential no longer maps to a user")
                    }
                    Err(why) => Response::err(&id, ErrorCode::Internal, why),
                },
            },
        };
        serde_json::to_value(resp).unwrap_or_else(|e| json!({"v": 1, "type": "response", "request_id": id, "ok": false,
            "error": {"code": "internal", "message": e.to_string()}}))
    }
}

impl Authenticator for Board {
    fn authenticate(&self, bearer_token: &str) -> Option<RemotePrincipal> {
        match self.check(bearer_token) {
            Authentication::Valid(p) => Some(p),
            Authentication::Invalid | Authentication::Unavailable(_) => None,
        }
    }

    /// A storage failure is Unavailable, never Invalid: it must not stop a valid bridge.
    fn check(&self, bearer_token: &str) -> Authentication {
        let digest = conde_remote::token_digest(bearer_token);
        match self.d.with_conn(|c| ops::hosts::authenticate(c, &digest)) {
            // The browser's reserved host is never a host credential, even one minted before
            // the name was reserved.
            Ok(Some((_, host_id)))
                if host_id == conde_remote::web::BROWSER_HOST || host_id == conde_remote::SLACK_INSTANCE =>
            {
                Authentication::Invalid
            }
            Ok(Some((user_id, host_id))) => {
                Authentication::Valid(RemotePrincipal { user_id: user_id.to_string(), host_id })
            }
            Ok(None) => Authentication::Invalid,
            Err(e) => Authentication::Unavailable(e.to_string()),
        }
    }
}

impl Board {
    /// One `bridge.claim`: every open attempt of the instance as a notify (with the structured
    /// fields on v2), plus the host's routes when the claim carries them.
    fn claim(&self, host_id: &str, bridge_instance: &str, body: Value) -> Polled {
        let claim = match self.bridge_op(host_id, bridge_instance, "bridge.claim", body) {
            Ok(c) => c,
            Err(e) => {
                if let OpFailure::Refused(_) = e
                    && let Ok(Some(newer)) = self.d.with_conn(|tx| crate::dispatch::fenced_by(tx, host_id, bridge_instance))
                {
                    tracing::info!(host = host_id, instance = bridge_instance, %newer, "poll found the instance fenced");
                    return Polled::Fenced;
                }
                tracing::warn!(host = host_id, instance = bridge_instance, "claim failed: {e:?}");
                return Polled::Due(Vec::new());
            }
        };
        let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let notifies = claim["open"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|j| {
                Some(Notify {
                    notification_id: s(j, "notification_id"),
                    attempt_id: s(j, "attempt_id"),
                    recipient: s(j, "recipient"),
                    binding_version: j["binding_version"].as_u64().unwrap_or_default(),
                    endpoint: endpoint_of(j["address"].as_str()?)?,
                    message_id: s(j, "message_id"),
                    text: s(j, "text"),
                    fields: j.get("fields").and_then(|f| serde_json::from_value::<NotifyFields>(f.clone()).ok()),
                })
            })
            .collect();
        match claim.get("routes").map(|r| serde_json::from_value::<Vec<Route>>(r.clone())) {
            Some(Ok(mailboxes)) => {
                Polled::Routed { notifies, routes: Routes { mailboxes, revision: claim["routes_revision"].as_u64() } }
            }
            Some(Err(e)) => {
                tracing::error!(host = host_id, instance = bridge_instance, "unreadable routes in a claim: {e}");
                Polled::Due(notifies)
            }
            None => Polled::Due(notifies),
        }
    }
}

fn endpoint_of(address: &str) -> Option<Endpoint> {
    let a = Address::parse(address, "-").ok()?;
    match a.kind {
        Kind::Claude => Some(Endpoint::Claude { session_id: a.target }),
        Kind::Codex => Some(Endpoint::Codex { thread_id: a.target }),
        Kind::Mailbox => Some(Endpoint::Mailbox { mailbox_id: a.target }),
        Kind::Human => None,
    }
}

impl NotificationSource for Board {
    fn pending_for_host(&self, host_id: &str, bridge_instance: &str) -> Vec<Notify> {
        match self.poll(host_id, bridge_instance) {
            Polled::Due(due) | Polled::Routed { notifies: due, .. } => due,
            Polled::Fenced => Vec::new(),
        }
    }

    /// A refused claim on a durably fenced instance is reported as such, so the
    /// server closes the connection even when the fence event was missed.
    fn poll(&self, host_id: &str, bridge_instance: &str) -> Polled {
        self.claim(host_id, bridge_instance, json!({"limit": 100, "open": true}))
    }

    /// A v2 instance's claim: within its window, with the structured fields and the host's routes.
    fn poll_window(&self, host_id: &str, bridge_instance: &str, window: u32) -> Polled {
        self.claim(host_id, bridge_instance, json!({"window": window, "open": true}))
    }

    /// The hello's protocol is checked against the host's migration state in the fence's own
    /// transaction, so a migration cannot slip between the check and the fence.
    fn admit(&self, host_id: &str, hello: &Hello) -> Admission {
        let instance = &hello.bridge_instance;
        match self.bridge_op(host_id, instance, "bridge.fence", json!({"proto": hello.proto})) {
            Ok(r) if r["active"] == true => {
                if let Some(old) = r["fenced"].as_array().filter(|a| !a.is_empty()) {
                    tracing::info!(host = host_id, instance, ?old, "bridge took over the host");
                }
                Admission::Active
            }
            Ok(r) => match r["refused"].as_str() {
                Some(why) => {
                    tracing::warn!(host = host_id, instance, proto = hello.proto, "hello refused: {why}");
                    Admission::Refused { code: "bad_hello".into(), message: why.to_string() }
                }
                None => Admission::Fenced,
            },
            Err(OpFailure::Refused(why)) => {
                tracing::error!(host = host_id, instance, "fence refused: {why}");
                Admission::Fenced
            }
            Err(OpFailure::Transient(why)) => unanswered(format!("fence of {instance} not recorded: {why}")),
        }
    }

    fn record_outcome(&self, host_id: &str, bridge_instance: &str, o: &Outcome) -> Recorded {
        let body = json!({"attempt_id": o.attempt_id, "outcome": o.status, "reason": o.reason, "event_seq": o.event_seq});
        match self.bridge_op(host_id, bridge_instance, "bridge.report", body) {
            Ok(r) if r["ignored"] == "unknown attempt" => Recorded::Rejected("unknown attempt".into()),
            Ok(_) => Recorded::Accepted,
            Err(OpFailure::Refused(why)) => Recorded::Rejected(why),
            // Never final: the bridge keeps the outcome and resends it on its timer.
            Err(OpFailure::Transient(why)) => Recorded::Retry(format!("outcome {} not recorded: {why}", o.attempt_id)),
        }
    }

    fn fence(&self, host_id: &str, bridge_instance: &str) -> FenceResult {
        match self.bridge_op(host_id, bridge_instance, "bridge.fence", json!({})) {
            Ok(r) if r["active"] == true => {
                if let Some(old) = r["fenced"].as_array().filter(|a| !a.is_empty()) {
                    tracing::info!(host = host_id, instance = bridge_instance, ?old, "bridge took over the host");
                }
                FenceResult::Active
            }
            Ok(_) => FenceResult::Fenced,
            Err(OpFailure::Refused(why)) => {
                tracing::error!(host = host_id, instance = bridge_instance, "fence refused: {why}");
                FenceResult::Fenced
            }
            // Fenced would stop the bridge for good; an unanswered hello makes it reconnect.
            Err(OpFailure::Transient(why)) => unanswered(format!("fence of {bridge_instance} not recorded: {why}")),
        }
    }

    /// The store's wake-ups (in-process for SQLite, LISTEN/NOTIFY across replicas for Postgres).
    fn events(&self) -> Option<broadcast::Receiver<SourceEvent>> {
        let mut wakes = self.d.store().pending().events();
        let (tx, rx) = broadcast::channel(256);
        tokio::spawn(async move {
            loop {
                let event = match wakes.recv().await {
                    Ok(Wake::Host(host)) => SourceEvent::Pending(Some(host)),
                    Ok(Wake::All) | Err(broadcast::error::RecvError::Lagged(_)) => SourceEvent::Pending(None),
                    Ok(Wake::Fenced(instance)) => SourceEvent::Fenced(instance),
                    Ok(Wake::Revoked(host)) => SourceEvent::Revoked(host),
                    Ok(Wake::Board(topics)) => SourceEvent::Board(topics),
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Some(rx)
    }

    /// The fleet's online state: the active instance's heartbeat, on every answered ping.
    fn heartbeat(&self, host_id: &str, bridge_instance: &str) {
        let at = iso_from_ms(self.d.now_ms());
        match self.d.with_tx(|tx| Ok(tx.touch_bridge(bridge_instance, &at)?)) {
            Ok(true) => {}
            Ok(false) => tracing::debug!(host = host_id, instance = bridge_instance, "heartbeat of an inactive instance"),
            Err(e) => tracing::warn!(host = host_id, instance = bridge_instance, "heartbeat not recorded: {e}"),
        }
    }

    fn endpoints(&self, host_id: &str, bridge_instance: &str, endpoints: &[Endpoint]) -> Recorded {
        let list: Vec<Value> = endpoints
            .iter()
            .map(|e| match e {
                Endpoint::Claude { session_id } => json!({"kind": "claude", "target": session_id}),
                Endpoint::Codex { thread_id } => json!({"kind": "codex", "target": thread_id}),
                // Taken only from a host migrated to v2 (bridge.endpoints decides).
                Endpoint::Mailbox { mailbox_id } => json!({"kind": "mailbox", "target": mailbox_id}),
            })
            .collect();
        match self.bridge_op(host_id, bridge_instance, "bridge.endpoints", json!({"endpoints": list})) {
            Ok(r) => {
                if r["abandoned"].as_array().is_some_and(|a| !a.is_empty()) {
                    tracing::info!(host = host_id, abandoned = %r["abandoned"], "holds on stopped sessions abandoned");
                }
                Recorded::Accepted
            }
            Err(OpFailure::Refused(why)) => {
                tracing::warn!(host = host_id, instance = bridge_instance, "endpoints refused: {why}");
                Recorded::Rejected(why)
            }
            Err(OpFailure::Transient(why)) => {
                tracing::warn!(host = host_id, instance = bridge_instance, "endpoints not recorded: {why}");
                Recorded::Retry(why)
            }
        }
    }
}

/// The shared board's advertised name (`CONDE_BOARD_NAME`, default `shared`): the alias every
/// teammate is offered at enrollment.
pub fn board_name() -> anyhow::Result<String> {
    let raw = std::env::var("CONDE_BOARD_NAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "shared".into());
    let name = crate::address::normalize_name(&raw)
        .filter(|n| !n.contains('.') && n != crate::protocol::v2::LOCAL_BOARD)
        .ok_or_else(|| anyhow::anyhow!("CONDE_BOARD_NAME '{raw}' is not a board name (one word, not 'local')"))?;
    Ok(name)
}

/// The lowest bridge protocol the server takes (`CONDE_MIN_PROTO`, default 1); 2 once every
/// enrolled host migrated.
pub fn min_proto() -> anyhow::Result<u32> {
    match std::env::var("CONDE_MIN_PROTO").ok().filter(|v| !v.is_empty()).as_deref() {
        None | Some("1") => Ok(conde_remote::PROTO_V1),
        Some("2") => Ok(conde_remote::PROTO_V2),
        Some(other) => anyhow::bail!("CONDE_MIN_PROTO '{other}' is not 1 or 2"),
    }
}

/// Start the remote listener over `d` (tests and `conde server`); `d` serves as the shared
/// board named by [`board_name`].
pub async fn start(d: Arc<Dispatcher>, listen: SocketAddr, config: ServerConfig) -> std::io::Result<ServerHandle> {
    // A bad board name is refused before the address is bound.
    board_name().map_err(std::io::Error::other)?;
    start_on(d, tokio::net::TcpListener::bind(listen).await?, config).await
}

/// As [`start`], on a listener the caller already bound.
pub async fn start_on(
    d: Arc<Dispatcher>,
    listener: tokio::net::TcpListener,
    config: ServerConfig,
) -> std::io::Result<ServerHandle> {
    d.share(&board_name().map_err(std::io::Error::other)?);
    // Every owner has exactly one human name before the board answers anyone.
    let d2 = d.clone();
    tokio::task::spawn_blocking(move || d2.with_tx(|tx| Ok(crate::humans::ensure_all(tx)?)))
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
    let board = Arc::new(Board::new(d));
    conde_remote::start_on(listener, config, board.clone(), board.clone(), board).await
}

/// `conde server`: serve until SIGTERM / SIGINT, then close within the grace.
pub fn run(d: Arc<Dispatcher>, listen: SocketAddr) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let dist_base_url = std::env::var("CONDE_DIST_BASE_URL").ok().filter(|u| !u.is_empty());
        let web = conde_remote::web::WebSetup::from_env(Arc::new(crate::web_store::WebBoard::new(d.clone())))?;
        if let Some(w) = &web {
            tracing::info!(entra = w.entra.is_some(), admin = w.admin_key.is_some(), "web surface enabled");
        }
        let public_url = web.as_ref().and_then(|w| w.entra.as_ref()).map(|c| c.public_url.clone());
        let slack = crate::slack::SlackConfig::from_env(&board_name()?, public_url)?;
        let min_proto = min_proto()?;
        tracing::info!(board = %board_name()?, min_proto, "shared board");
        let handle =
            start(d.clone(), listen, ServerConfig { dist_base_url, web, min_proto, ..ServerConfig::default() }).await?;
        if let Some(config) = slack {
            tracing::info!(people = config.members.len(), "slack notifications on");
            crate::slack::spawn(d, Arc::new(config))?;
        }
        tracing::info!(addr = %handle.local_addr(), "conde server ready");
        crate::daemon::termination().await;
        tracing::info!("terminating: closing bridge connections");
        match tokio::time::timeout(SHUTDOWN_GRACE, handle.shutdown()).await {
            Ok(r) => r?,
            Err(_) => tracing::warn!("connections still open after {SHUTDOWN_GRACE:?}; exiting"),
        }
        Ok(())
    })
}
