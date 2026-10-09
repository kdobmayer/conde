//! Cloud mode, server side: the board behind conde-remote's listener. Every
//! piece of state (credentials, fencing, attempts, endpoints) lives in the
//! store and is reached through the dispatcher's ops, so a restarted server
//! loses or duplicates nothing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use conde_remote::{
    Authentication, Authenticator, Endpoint, FenceResult, NotificationSource, Notify, Outcome, Polled, Principal as RemotePrincipal,
    Recorded, ServerConfig, ServerHandle, SourceEvent, WebPrincipal,
};
use tokio::sync::broadcast;
use serde_json::{Value, json};

use crate::address::{Address, Kind};
use crate::dispatch::{Dispatcher, Principal};
use crate::ops;
use crate::protocol::{ErrorCode, Request, Response};
use crate::store::Wake;
use crate::util::{iso_from_ms, new_ulid};

/// The host of a human first named from the browser (`human:<user>@web`): a user whose human
/// has no name on the board yet gets one there. Not a real host; nothing is delivered to it.
pub const WEB_HOST: &str = conde_remote::web::BROWSER_HOST;

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

    fn dispatch_web(&self, user: &WebPrincipal, request: Value) -> Value {
        let id = request.get("request_id").and_then(Value::as_str).unwrap_or_default().to_string();
        let resp = match serde_json::from_value::<Request>(request) {
            Err(e) => Response::err(&id, ErrorCode::BadRequest, format!("bad request frame: {e}")),
            Ok(req) => match self.web_human(user) {
                Ok((p, actor, name)) => {
                    self.d.dispatch(&p, &Request { actor: Some(actor), from: Some(name), ..req })
                }
                Err((code, message)) => Response::err(&id, code, message),
            },
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

impl Board {
    /// The principal, actor and name a browser user acts as: their human, a name bound to a
    /// `human:` address in their namespace. A name registered from a terminal wins over the
    /// browser's own; among several, the one named after the user. The browser acts by name
    /// (the terminal human's `--from`), so two names on one human address stay usable. With
    /// `register`, a user without a human gets `<user>` bound to `human:<user>@web` through the
    /// CLI's registration rules, unless an agent session or a pin holds that name.
    fn web_human(&self, user: &WebPrincipal) -> Result<(Principal, String, String), (ErrorCode, String)> {
        let internal = |e: crate::store::StoreError| (ErrorCode::Internal, format!("storage error: {e}"));
        let (row, humans, same_name) = self
            .d
            .with_conn(|tx| {
                let row = tx.user_by_id(user.user_id)?;
                let humans: Vec<_> =
                    tx.bound_agents()?.into_iter().filter(|a| a.owner_id == user.user_id && a.is_human()).collect();
                let same_name = match &row {
                    Some(r) => tx.agent_by_name(r.user_id, &r.name)?,
                    None => None,
                };
                Ok((row, humans, same_name))
            })
            .map_err(internal)?;
        let Some(row) = row.filter(|r| !r.disabled) else {
            return Err((ErrorCode::Unauthenticated, "this account is disabled or gone".into()));
        };
        let principal = |host: &str| Principal {
            user_id: row.user_id,
            user: row.name.clone(),
            host: host.to_string(),
            // Acts by name through `from`, which the dispatcher allows a human principal only.
            local: true,
            bridge_instance: None,
            bridge_connection: None,
        };
        // A human named from a terminal wins over the one the browser named on `web`.
        let on_web = |a: &&crate::store::Agent| a.address.as_ref().is_some_and(|x| x.host == WEB_HOST);
        let (web, named): (Vec<_>, Vec<_>) = humans.iter().partition(on_web);
        let chosen = named
            .iter()
            .find(|a| a.name == row.name)
            .or(if named.len() == 1 { named.first() } else { None })
            .or(if named.is_empty() { web.first() } else { None });
        if let Some(agent) = chosen
            && let Some(address) = &agent.address
        {
            return Ok((principal(&address.host), address.to_string(), agent.name.clone()));
        }
        if !humans.is_empty() {
            let names: Vec<String> = humans.iter().map(|a| a.canonical()).collect();
            return Err((
                ErrorCode::Conflict,
                format!(
                    "several names are bound to your human ({}); the browser acts as the one named '{}'",
                    names.join(", "),
                    row.name
                ),
            ));
        }
        let none_yet = |why: &str| {
            format!(
                "your human has no name on the board yet{why}. Register your human from a terminal with: \
                 conde register <name>"
            )
        };
        if let Some(agent) = &same_name
            && (agent.address.is_some() || agent.pinned.is_some())
        {
            let held = format!(", and the name '{}' belongs to {}", row.name, agent.address.as_ref().map_or_else(
                || "a pinned role".to_string(),
                |a| format!("the session {a}")
            ));
            return Err((ErrorCode::NotFound, none_yet(&held)));
        }
        if !user.register {
            return Err((ErrorCode::NotFound, none_yet("")));
        }
        let p = principal(WEB_HOST);
        let actor = Address::new(Kind::Human, &row.name, WEB_HOST).to_string();
        let mut register =
            Request::new(new_ulid(), "agent.register", json!({"name": row.name, "keep_sessions": true}));
        register.actor = Some(actor.clone());
        register.idempotency_key = Some(new_ulid());
        let resp = self.d.dispatch(&Principal { local: false, ..p.clone() }, &register);
        match resp.error {
            None => {
                tracing::info!(user = %row.name, %actor, "named the browser human");
                Ok((p, actor, row.name.clone()))
            }
            Some(e) => Err((e.code, none_yet(&format!(", and naming it '{}' failed: {}", row.name, e.message)))),
        }
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
