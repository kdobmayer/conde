//! Cloud mode, server side: the board behind conde-remote's listener. Every
//! piece of state (credentials, fencing, attempts, endpoints) lives in the
//! store and is reached through the dispatcher's ops, so a restarted server
//! loses or duplicates nothing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use conde_remote::{
    Authentication, Authenticator, Endpoint, FenceResult, NotificationSource, Notify, Outcome, Polled, Principal as RemotePrincipal,
    Recorded, ServerConfig, ServerHandle, SourceEvent,
};
use tokio::sync::broadcast;
use serde_json::{Value, json};

use crate::address::{Address, Kind};
use crate::dispatch::{Dispatcher, Principal};
use crate::ops;
use crate::protocol::{ErrorCode, Request, Response};
use crate::store::Wake;
use crate::util::new_ulid;

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
            Ok(Some((user_id, host_id))) => {
                Authentication::Valid(RemotePrincipal { user_id: user_id.to_string(), host_id })
            }
            Ok(None) => Authentication::Invalid,
            Err(e) => Authentication::Unavailable(e.to_string()),
        }
    }
}

fn endpoint_of(address: &str) -> Option<Endpoint> {
    let a = Address::parse(address, "-").ok()?;
    match a.kind {
        Kind::Claude => Some(Endpoint::Claude { session_id: a.target }),
        Kind::Codex => Some(Endpoint::Codex { thread_id: a.target }),
        Kind::Human => None,
    }
}

impl NotificationSource for Board {
    fn pending_for_host(&self, host_id: &str, bridge_instance: &str) -> Vec<Notify> {
        match self.poll(host_id, bridge_instance) {
            Polled::Due(due) => due,
            Polled::Fenced => Vec::new(),
        }
    }

    /// A refused claim on a durably fenced instance is reported as such, so the
    /// server closes the connection even when the fence event was missed.
    fn poll(&self, host_id: &str, bridge_instance: &str) -> Polled {
        let claim = match self.bridge_op(host_id, bridge_instance, "bridge.claim", json!({"limit": 100, "open": true})) {
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
        Polled::Due(claim["open"]
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
                })
            })
            .collect())
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
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Some(rx)
    }

    fn endpoints(&self, host_id: &str, bridge_instance: &str, endpoints: &[Endpoint]) {
        let list: Vec<Value> = endpoints
            .iter()
            .map(|e| match e {
                Endpoint::Claude { session_id } => json!({"kind": "claude", "target": session_id}),
                Endpoint::Codex { thread_id } => json!({"kind": "codex", "target": thread_id}),
            })
            .collect();
        match self.bridge_op(host_id, bridge_instance, "bridge.endpoints", json!({"endpoints": list})) {
            Ok(r) if r["abandoned"].as_array().is_some_and(|a| !a.is_empty()) => {
                tracing::info!(host = host_id, abandoned = %r["abandoned"], "holds on stopped sessions abandoned");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(host = host_id, instance = bridge_instance, "endpoints not recorded: {e:?}"),
        }
    }
}

/// Start the remote listener over `d` (tests and `conde server`).
pub async fn start(d: Arc<Dispatcher>, listen: SocketAddr, config: ServerConfig) -> std::io::Result<ServerHandle> {
    let board = Arc::new(Board::new(d));
    conde_remote::start(listen, config, board.clone(), board.clone(), board).await
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
        let handle = start(d, listen, ServerConfig { dist_base_url, web, ..ServerConfig::default() }).await?;
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
