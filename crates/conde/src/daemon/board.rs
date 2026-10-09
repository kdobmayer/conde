//! One shared board's connection (DESIGN-v2.md §5): a v2 bridge client, its reconciliation
//! of routes, the resumable board ops, and the task that hands outcomes to the client's book.
//! A board that is fenced, revoked or refused stops alone; the daemon keeps running.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use conde_remote::{BridgeClient, BridgeConfig, BridgeState, CallError, Inbound, PROTO_V2, Route, StopReason};
use serde_json::{Value, json};

use super::Core;
use crate::boards::BoardConfig;
use crate::protocol::v2::{Mine, Published, op};
use crate::protocol::{ErrorCode, Response};
use crate::store::Tx;
use crate::store::local::{BoardOp, BoardOpKind, BoardOpState, LocalTx, PublicationState};
use crate::util::{iso_from_ms, now_ms};

/// How often a board's task hands outcomes to the book and checks its connection.
const DRAIN_EVERY: Duration = Duration::from_millis(20);

/// Where a board's connection stands, for `doctor` and the resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Enrolled, but its one-time migration is not confirmed: no hello is sent.
    NotMigrated,
    /// The connection runs (connected or reconnecting; see the client's state).
    Running,
    /// Stopped for good (fenced, credential rejected, hello refused); the daemon runs on.
    Stopped(String),
    /// The board file cannot be used.
    Broken(String),
}

pub struct BoardConn {
    pub alias: String,
    pub config: Option<BoardConfig>,
    pub status: Mutex<Status>,
    client: Mutex<Option<BridgeClient>>,
    /// Publications the board no longer routes here, removed by reconciliation (doctor).
    pub removed: Mutex<Vec<String>>,
    /// The last reconciliation failure, if any (doctor).
    pub note: Mutex<Option<String>>,
}

impl BoardConn {
    pub fn new(alias: &str, config: anyhow::Result<BoardConfig>) -> BoardConn {
        let (config, status) = match config {
            Ok(c) => (Some(c), Status::NotMigrated),
            Err(e) => (None, Status::Broken(format!("{e:#}"))),
        };
        BoardConn {
            alias: alias.to_string(),
            config,
            status: Mutex::new(status),
            client: Mutex::new(None),
            removed: Mutex::default(),
            note: Mutex::default(),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set_status(&self, s: Status) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = s;
    }

    pub fn client(&self) -> Option<BridgeClient> {
        self.client.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Connected right now (a request can be answered without waiting for a reconnect).
    pub fn connected(&self) -> bool {
        self.client().is_some_and(|c| c.state() == BridgeState::Connected)
    }

    pub fn host(&self) -> String {
        self.config.as_ref().map(|c| c.host_id.clone()).unwrap_or_default()
    }

    pub fn owner(&self) -> String {
        self.config.as_ref().map(|c| c.owner.clone()).unwrap_or_default()
    }

    pub fn shutdown(&self) {
        if let Some(c) = self.client() {
            c.shutdown();
        }
    }

    /// One request over the connection, with the board's answer as a response object.
    pub async fn call(&self, request: Value, wait: Duration) -> Result<Response, String> {
        let Some(client) = self.client() else {
            return Err(match self.status() {
                Status::NotMigrated => format!("{}: not migrated (run conde setup)", self.alias),
                Status::Stopped(why) | Status::Broken(why) => format!("{}: {why}", self.alias),
                Status::Running => format!("{}: not connected", self.alias),
            });
        };
        match tokio::time::timeout(wait, client.call(request)).await {
            Ok(Ok(v)) => serde_json::from_value(v).map_err(|e| format!("{}: bad response frame: {e}", self.alias)),
            Ok(Err(e)) => Err(format!("{}: {}", self.alias, describe_call(&e))),
            Err(_) => Err(format!("{}: no answer within {wait:?}", self.alias)),
        }
    }
}

fn describe_call(e: &CallError) -> String {
    match e {
        CallError::Stopped(reason) => describe_stop(reason),
        other => other.to_string(),
    }
}

/// The doctor's words for a stopped connection.
pub fn describe_stop(reason: &StopReason) -> String {
    match reason {
        StopReason::Fenced => "stopped: fenced by a newer bridge of this host (another daemon with its credential?)".into(),
        StopReason::Unauthorized => "stopped: the board rejected this host's credential (revoked?); re-enroll with conde login".into(),
        StopReason::Rejected { code, message } => format!("stopped: the board refused the connection ({code}: {message})"),
        StopReason::Shutdown => "stopped".into(),
    }
}

/// Starts the board's v2 connection and its task.
pub fn start(core: &Arc<Core>, conn: &Arc<BoardConn>) {
    let Some(cfg) = conn.config.clone() else { return };
    let url = match crate::credentials::bridge_url(&cfg.server) {
        Ok(u) => u,
        Err(e) => {
            conn.set_status(Status::Broken(format!("{e:#}")));
            return;
        }
    };
    let tuning = &core.mux.tuning;
    let mut config = BridgeConfig::new(url, cfg.key.clone(), cfg.host_id.clone());
    config.proto = PROTO_V2;
    config.window = Some(tuning.window);
    if let Some(t) = tuning.idle_timeout {
        config.idle_timeout = t;
    }
    if let Some(t) = tuning.outcome_ack_timeout {
        config.outcome_ack_timeout = t;
    }
    if let Some(n) = tuning.max_unacked_outcomes {
        config.max_unacked_outcomes = n;
    }
    if let Some(b) = tuning.backoff_max {
        config.backoff_max = b;
        config.backoff_min = config.backoff_min.min(b);
    }
    let alias = conn.alias.clone();
    let weak: Weak<Core> = Arc::downgrade(core);
    let _rt = core.rt.enter();
    let client = BridgeClient::start_events(config, move |event| {
        let Some(core) = weak.upgrade() else { return };
        match event {
            Inbound::Connected => core.mux.connected(&alias),
            Inbound::Routes(r) => {
                reconcile(&core, &alias, &r.mailboxes, None);
                core.mux.reconciled(&alias);
            }
            Inbound::Notify(n) => core.mux.push(&alias, n),
        }
    });
    tracing::info!(board = conn.alias, server = cfg.server, host = cfg.host_id, instance = client.bridge_instance(), "board connection starting");
    *conn.client.lock().unwrap_or_else(|p| p.into_inner()) = Some(client.clone());
    conn.set_status(Status::Running);
    core.mux.add_board(&conn.alias, client.clone());
    core.rt.spawn(run(Arc::downgrade(core), conn.clone(), client));
}

/// The board's task: per connection, resume its pending ops and reconcile its routes; hand
/// outcomes to the book whenever it has room; stop the origin when the client stops.
async fn run(core: Weak<Core>, conn: Arc<BoardConn>, client: BridgeClient) {
    let mut seen_connects = 0;
    let mut mine_due = false;
    let mut mine_retry = tokio::time::Instant::now();
    loop {
        let Some(core) = core.upgrade() else { return };
        if let BridgeState::Stopped(reason) = client.state() {
            let why = describe_stop(&reason);
            tracing::warn!(board = conn.alias, "{why}");
            core.mux.stop_board(&conn.alias, &why);
            conn.set_status(Status::Stopped(why));
            return;
        }
        if client.state() == BridgeState::Connected && client.connects() != seen_connects {
            seen_connects = client.connects();
            mine_due = true;
            let pending = core.local_tx(|tx| Ok(tx.pending_ops(Some(&conn.alias))?)).unwrap_or_default();
            for op in pending.into_iter().filter(|o| o.kind != BoardOpKind::Migrate) {
                let core = core.clone();
                let conn = conn.clone();
                tokio::spawn(async move {
                    let outcome = send_op(&conn, &op, core.mux.tuning.call_timeout).await;
                    if let Err((_, why)) = finish_op(&core, &conn, &op, outcome) {
                        tracing::warn!(board = conn.alias, op = op.op_id, "resumed board op refused: {why}");
                    }
                });
            }
        }
        if mine_due && tokio::time::Instant::now() >= mine_retry {
            let since = iso_from_ms(now_ms());
            match conn.call(json!({"op": op::MAILBOX_MINE, "body": {}}), core.mux.tuning.call_timeout).await {
                Ok(resp) if resp.ok => {
                    let mine: Mine = resp.result.and_then(|r| serde_json::from_value(r).ok()).unwrap_or_default();
                    let routes: Vec<Route> = mine
                        .mailboxes
                        .into_iter()
                        .map(|m| Route { mailbox_id: m.mailbox_id, route_generation: m.route_generation })
                        .collect();
                    reconcile(&core, &conn.alias, &routes, Some(&since));
                    core.mux.reconciled(&conn.alias);
                    mine_due = false;
                }
                Ok(resp) => {
                    let why = resp.error.map(|e| e.message).unwrap_or_default();
                    *conn.note.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("mailbox.mine refused: {why}"));
                    // Routes frames still reconcile; nothing better is known.
                    core.mux.reconciled(&conn.alias);
                    mine_due = false;
                }
                Err(why) => {
                    tracing::debug!(board = conn.alias, "mailbox.mine: {why}");
                    mine_retry = tokio::time::Instant::now() + Duration::from_secs(1);
                }
            }
        }
        drain(&core, &conn.alias, &client);
        drop(core);
        tokio::time::sleep(DRAIN_EVERY).await;
    }
}

/// Hands the board's outcomes to the client's book, in record order, while it has room.
fn drain(core: &Core, alias: &str, client: &BridgeClient) {
    for r in core.mux.outcomes(alias) {
        let reason = r.reason.map(|x| x.as_str());
        match client.report(&r.attempt_id, r.status.as_str(), reason) {
            Ok(_) => core.mux.accepted(alias, &r),
            Err(CallError::Backpressure) | Err(CallError::Stopped(_)) => return,
            Err(CallError::TooLarge { .. }) => {
                let too_large = conde_remote::OutcomeReason::OutcomeTooLarge.as_str();
                match client.report(&r.attempt_id, "failed", Some(too_large)) {
                    Ok(_) => core.mux.accepted(alias, &crate::daemon::mux::OutcomeRecord { last: true, ..r }),
                    Err(e) => {
                        tracing::error!(board = alias, "attempt not settled: {e}");
                        return;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(board = alias, "outcome not reported: {e}");
                return;
            }
        }
    }
}

/// Applies the board's current routes for this host: a published row the board no longer
/// routes here is deleted (and kept for doctor); a newer generation is adopted. Rows changed
/// after `since` (the snapshot's time, for `mailbox.mine`) are left alone.
pub fn reconcile(core: &Core, alias: &str, routes: &[Route], since: Option<&str>) {
    let now = iso_from_ms(now_ms());
    let result = core.local_tx(|tx| {
        let mut removed = Vec::new();
        for p in tx.publications(Some(alias))? {
            if p.state != PublicationState::Published || since.is_some_and(|s| p.updated_at.as_str() > s) {
                continue;
            }
            let Some(mailbox) = p.mailbox_id.as_deref() else { continue };
            match routes.iter().find(|r| r.mailbox_id == mailbox) {
                None => {
                    tx.delete_publication(alias, &p.agent_id)?;
                    let name = tx.agent_by_id(&p.agent_id)?.map(|a| a.name).unwrap_or_else(|| p.agent_id.clone());
                    removed.push(format!("{name} (as {}): no longer routed to this host", p.shared_name));
                }
                Some(r) if Some(r.route_generation) > p.route_generation => {
                    tx.update_publication(alias, &p.agent_id, Some((mailbox, r.route_generation)), PublicationState::Published, &now)?;
                }
                Some(_) => {}
            }
        }
        Ok(removed)
    });
    match result {
        Ok(removed) if !removed.is_empty() => {
            tracing::info!(board = alias, ?removed, "publications removed by the board's routes");
            if let Some(conn) = core.board(alias) {
                conn.removed.lock().unwrap_or_else(|p| p.into_inner()).extend(removed);
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!(board = alias, "reconciliation failed: {e:#}"),
    }
}

/// What one send of a board op came to.
pub enum OpOutcome {
    Done(Value),
    /// The board refused it for good.
    Refused(ErrorCode, String),
    /// Not answered (offline, lost response): stays pending and is resumed on connect.
    Pending(String),
}

/// Sends `op` with its stored body and its op id as idempotency key.
pub async fn send_op(conn: &BoardConn, op: &BoardOp, wait: Duration) -> OpOutcome {
    let request = json!({"op": op.kind.op_name(), "body": op.request, "idempotency_key": op.op_id});
    let response = if op.kind == BoardOpKind::Migrate {
        rpc(conn, request, wait).await
    } else {
        conn.call(request, wait).await
    };
    match response {
        Ok(r) if r.ok => OpOutcome::Done(r.result.unwrap_or(Value::Null)),
        Ok(r) => match r.error {
            Some(e) if e.code == ErrorCode::Internal => OpOutcome::Pending(e.message),
            Some(e) => OpOutcome::Refused(e.code, e.message),
            None => OpOutcome::Pending("empty response".into()),
        },
        Err(why) => OpOutcome::Pending(why),
    }
}

/// One request over the board's HTTPS RPC, outside the connection (no hello is sent).
pub async fn rpc(conn: &BoardConn, request: Value, wait: Duration) -> Result<Response, String> {
    let Some(cfg) = &conn.config else { return Err(format!("{}: no usable board file", conn.alias)) };
    let url = crate::credentials::rpc_url(&cfg.server).map_err(|e| format!("{e:#}"))?;
    match conde_remote::rpc(&url, &cfg.key, request, wait).await {
        Ok(v) => serde_json::from_value(v).map_err(|e| format!("{}: bad response: {e}", conn.alias)),
        Err(e) => Err(format!("{}: {}", conn.alias, describe_call(&e))),
    }
}

/// Applies a publish or unpublish answer locally (migrate answers are applied by
/// [`super::publish::apply_manifest`]). Returns the state the publication ended in.
pub fn finish_op(core: &Core, conn: &BoardConn, op: &BoardOp, outcome: OpOutcome) -> Result<String, (ErrorCode, String)> {
    let alias = &conn.alias;
    let agent = op.agent_id.clone().unwrap_or_default();
    let now = iso_from_ms(now_ms());
    match (op.kind, outcome) {
        (_, OpOutcome::Pending(why)) => {
            tracing::info!(board = alias, op = op.op_id, "board op pending: {why}");
            Ok("pending".into())
        }
        (BoardOpKind::Publish, OpOutcome::Done(result)) => {
            let published: Option<Published> = serde_json::from_value(result.clone()).ok();
            core.local_tx(|tx| {
                if let Some(p) = &published
                    && tx.publication(alias, &agent)?.is_some()
                {
                    tx.update_publication(alias, &agent, Some((&p.mailbox_id, p.route_generation)), PublicationState::Published, &now)?;
                }
                tx.finish_op(&op.op_id, BoardOpState::Done, Some(&result), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Ok("published".into())
        }
        (BoardOpKind::Publish, OpOutcome::Refused(code, message)) => {
            core.local_tx(|tx| {
                if tx.publication(alias, &agent)?.is_some_and(|p| p.state == PublicationState::Publishing) {
                    tx.delete_publication(alias, &agent)?;
                }
                tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&json!({"error": {"code": code, "message": message}})), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Err((code, format!("{alias}: {message}")))
        }
        (BoardOpKind::Unpublish, OpOutcome::Done(result)) => {
            core.local_tx(|tx| {
                tx.delete_publication(alias, &agent)?;
                tx.finish_op(&op.op_id, BoardOpState::Done, Some(&result), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Ok("unpublished".into())
        }
        (BoardOpKind::Unpublish, OpOutcome::Refused(code, message))
            if matches!(code, ErrorCode::Conflict | ErrorCode::NotFound) =>
        {
            // The mailbox moved (or is gone) meanwhile: the local row goes, the new route stays.
            core.local_tx(|tx| {
                tx.delete_publication(alias, &agent)?;
                tx.finish_op(&op.op_id, BoardOpState::Done, Some(&json!({"moved": true, "message": message})), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Ok("moved".into())
        }
        (BoardOpKind::Unpublish, OpOutcome::Refused(code, message)) => {
            let mailbox = op.request["mailbox_id"].as_str().unwrap_or_default().to_string();
            let generation = op.request["expected_generation"].as_u64().unwrap_or_default();
            core.local_tx(|tx| {
                if tx.publication(alias, &agent)?.is_some() {
                    tx.update_publication(alias, &agent, Some((&mailbox, generation)), PublicationState::Published, &now)?;
                }
                tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&json!({"error": {"code": code, "message": message}})), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Err((code, format!("{alias}: {message}")))
        }
        (BoardOpKind::Migrate, OpOutcome::Done(result)) => super::publish::apply_manifest(core, conn, op, &result),
        (BoardOpKind::Migrate, OpOutcome::Refused(code, message)) => {
            core.local_tx(|tx| {
                tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&json!({"error": {"code": code, "message": message}})), &now)?;
                Ok(())
            })
            .map_err(|e| (ErrorCode::Internal, format!("{e:#}")))?;
            Err((code, format!("{alias}: {message}")))
        }
    }
}

/// Resumes the board's pending migrate op (over HTTPS), then connects when it is done.
pub async fn resume_migration(core: Arc<Core>, conn: Arc<BoardConn>, op: BoardOp) {
    let mut delay = Duration::from_secs(1);
    loop {
        let outcome = send_op(&conn, &op, core.mux.tuning.call_timeout).await;
        match outcome {
            OpOutcome::Pending(why) => {
                tracing::warn!(board = conn.alias, "migration pending: {why}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(60));
            }
            other => {
                if finish_op(&core, &conn, &op, other).is_ok() {
                    start(&core, &conn);
                }
                return;
            }
        }
    }
}

/// The local agent's publication row on a board, by agent id.
pub fn publication_of(core: &Core, alias: &str, agent_id: &str) -> Option<crate::store::local::Publication> {
    core.local_tx(|tx| Ok(tx.publication(alias, agent_id)?)).ok().flatten()
}
