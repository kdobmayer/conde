//! One shared board's connection (DESIGN-v2.md §5): a v2 bridge client, its reconciliation
//! of routes, the resumable board ops, and the task that hands outcomes to the client's book.
//! A board that is fenced, revoked or refused stops alone; the daemon keeps running.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use conde_remote::{BridgeClient, BridgeConfig, BridgeState, CallError, Inbound, PROTO_V2, Route, StopReason};
use serde_json::{Value, json};

use super::Core;
use crate::boards::BoardConfig;
use crate::protocol::v2::{BoardInfo, Mine, Published, op};
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
    /// Per local agent: the frame order (see `BridgeClient::call_ordered`) of the response
    /// that last completed its publication; a routes snapshot received earlier never undoes it.
    completions: Mutex<HashMap<String, u64>>,
    /// Connections seen by the callback; a `mailbox.mine` answer reconciles only the
    /// connection it was asked on.
    epoch: AtomicU64,
    /// A connection started and its `mailbox.mine` is not answered yet.
    mine_due: AtomicBool,
    /// The newest route revision applied (from a snapshot or a mutation's answer); an older
    /// snapshot is ignored.
    revision: Mutex<u64>,
    /// `board.forget` removed this board: nothing applies to it or starts it any more.
    pub forgotten: AtomicBool,
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
            completions: Mutex::default(),
            epoch: AtomicU64::new(0),
            mine_due: AtomicBool::new(false),
            revision: Mutex::new(0),
            forgotten: AtomicBool::new(false),
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
        self.call_ordered(request, wait).await.map(|(r, _)| r)
    }

    /// [`call`](Self::call) with the response's place in the order of received frames.
    pub async fn call_ordered(&self, request: Value, wait: Duration) -> Result<(Response, u64), String> {
        let Some(client) = self.client() else {
            return Err(match self.status() {
                Status::NotMigrated => format!("{}: not migrated (run conde setup)", self.alias),
                Status::Stopped(why) | Status::Broken(why) => format!("{}: {why}", self.alias),
                Status::Running => format!("{}: not connected", self.alias),
            });
        };
        match tokio::time::timeout(wait, client.call_ordered(request)).await {
            Ok(Ok((v, seq))) => serde_json::from_value(v)
                .map(|r| (r, seq))
                .map_err(|e| format!("{}: bad response frame: {e}", self.alias)),
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
    if conn.forgotten.load(Ordering::SeqCst) {
        return;
    }
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
    if let Some(t) = tuning.outcome_resend {
        config.outcome_resend_min = t;
        config.outcome_resend_max = t;
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
            Inbound::Connected => {
                // In callback order: the reset, then the request that reconciles this connection.
                core.mux.connected(&alias);
                if let Some(conn) = core.board(&alias) {
                    conn.epoch.fetch_add(1, Ordering::SeqCst);
                    conn.mine_due.store(true, Ordering::SeqCst);
                }
            }
            Inbound::Routes { routes, seq } => {
                if let Some(d) = core.mux.tuning.routes_delay {
                    std::thread::sleep(d);
                }
                reconcile(&core, &alias, &routes.mailboxes, seq, routes.revision);
                core.mux.reconciled(&alias);
            }
            Inbound::Notify(n) => core.mux.push(&alias, *n),
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
        if tokio::time::Instant::now() >= mine_retry && conn.mine_due.swap(false, Ordering::SeqCst) {
            let epoch = conn.epoch.load(Ordering::SeqCst);
            let current = |conn: &BoardConn| conn.epoch.load(Ordering::SeqCst) == epoch;
            match conn.call_ordered(json!({"op": op::MAILBOX_MINE, "body": {}}), core.mux.tuning.call_timeout).await {
                Ok((resp, seq)) if resp.ok => {
                    let mine: Mine = resp.result.and_then(|r| serde_json::from_value(r).ok()).unwrap_or_default();
                    let routes: Vec<Route> = mine
                        .mailboxes
                        .into_iter()
                        .map(|m| Route { mailbox_id: m.mailbox_id, route_generation: m.route_generation })
                        .collect();
                    reconcile(&core, &conn.alias, &routes, seq, mine.revision);
                    if current(&conn) {
                        core.mux.reconciled(&conn.alias);
                    }
                }
                Ok((resp, _)) => {
                    let why = resp.error.map(|e| e.message).unwrap_or_default();
                    *conn.note.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("mailbox.mine refused: {why}"));
                    // Routes frames still reconcile; nothing better is known.
                    if current(&conn) {
                        core.mux.reconciled(&conn.alias);
                    }
                }
                Err(why) => {
                    tracing::debug!(board = conn.alias, "mailbox.mine: {why}");
                    if current(&conn) {
                        conn.mine_due.store(true, Ordering::SeqCst);
                    }
                    mine_retry = tokio::time::Instant::now() + Duration::from_millis(200);
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

/// Applies a routes snapshot received as frame `seq`: a published row the board no longer
/// routes here is deleted (and kept for doctor); a newer generation is adopted. A row whose
/// publication completed by a response received after the snapshot is left alone: the snapshot
/// predates it, whatever order the two were handled in.
pub fn reconcile(core: &Core, alias: &str, routes: &[Route], seq: u64, revision: Option<u64>) {
    let now = iso_from_ms(now_ms());
    let conn = core.board(alias);
    let result = core.local_tx(|tx| {
        // Read under the store's lock: a completion is recorded before its transaction commits.
        let completions =
            conn.as_ref().map(|c| c.completions.lock().unwrap_or_else(|p| p.into_inner()).clone()).unwrap_or_default();
        let mut removed = Vec::new();
        // Causality first: a snapshot read at an older route revision than one already
        // applied (a snapshot, or a publish or unpublish answer) is stale as a whole.
        if let (Some(conn), Some(r)) = (&conn, revision) {
            let mut applied = conn.revision.lock().unwrap_or_else(|p| p.into_inner());
            if r < *applied {
                tracing::info!(board = alias, revision = r, applied = *applied, "stale routes snapshot ignored");
                return Ok(removed);
            }
            *applied = r;
        }
        for p in tx.publications(Some(alias))? {
            if p.state != PublicationState::Published || completions.get(&p.agent_id).is_some_and(|done| *done > seq) {
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
    /// The board's answer, with the frame order of its response (None over HTTPS).
    Done(Value, Option<u64>),
    /// The board refused it for good.
    Refused(ErrorCode, String),
    /// Not answered (offline, lost response): stays pending and is resumed on connect.
    Pending(String),
}

/// Sends `op` with its stored body and its op id as idempotency key.
pub async fn send_op(conn: &BoardConn, op: &BoardOp, wait: Duration) -> OpOutcome {
    let request = json!({"op": op.kind.op_name(), "body": op.request, "idempotency_key": op.op_id});
    let response = if op.kind == BoardOpKind::Migrate {
        rpc(conn, request, wait).await.map(|r| (r, None))
    } else {
        conn.call_ordered(request, wait).await.map(|(r, seq)| (r, Some(seq)))
    };
    match response {
        Ok((r, seq)) if r.ok => OpOutcome::Done(r.result.unwrap_or(Value::Null), seq),
        Ok((r, _)) => match r.error {
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

/// What an op that is no longer pending already came to, from its stored record.
fn settled(op: &BoardOp) -> Result<String, (ErrorCode, String)> {
    match (op.kind, op.state) {
        (_, BoardOpState::Failed) => {
            let message = op.result.as_ref().and_then(|r| r["error"]["message"].as_str()).unwrap_or("refused").to_string();
            Err((ErrorCode::Conflict, format!("{}: {message}", op.board)))
        }
        (BoardOpKind::Publish, _) => Ok("published".into()),
        (BoardOpKind::Unpublish, _) => Ok("unpublished".into()),
        (BoardOpKind::Migrate, _) => Ok("migrated".into()),
    }
}

/// Applies a board's answer to `op` locally, once: the originating request and the reconnect
/// worker may both finish the same op, so the op's state is reread inside the transaction and
/// an op that is no longer pending returns its stored result and changes nothing. A transition
/// touches only the row state its own op created (`publishing` for a publish, `unpublishing`
/// for an unpublish), never a newer one. Migrate answers go to
/// [`super::publish::apply_manifest`]. Returns the state the publication ended in.
pub fn finish_op(core: &Core, conn: &BoardConn, op: &BoardOp, outcome: OpOutcome) -> Result<String, (ErrorCode, String)> {
    let alias = &conn.alias;
    let agent = op.agent_id.clone().unwrap_or_default();
    let now = iso_from_ms(now_ms());
    let internal = |e: anyhow::Error| (ErrorCode::Internal, format!("{e:#}"));
    // The answer when this completion must change nothing: the board was forgotten (its op
    // is gone), or the op already ended (a duplicate completion).
    let stored = |tx: &crate::store::sqlite::SqliteTx| -> anyhow::Result<Option<Result<String, (ErrorCode, String)>>> {
        if conn.forgotten.load(Ordering::SeqCst) {
            return Ok(Some(Ok("forgotten".into())));
        }
        Ok(match tx.op(&op.op_id)? {
            None => Some(Ok("forgotten".into())),
            Some(o) if o.state != BoardOpState::Pending => Some(settled(&o)),
            Some(_) => None,
        })
    };
    let advance = |r: Option<u64>| {
        if let Some(r) = r {
            let mut applied = conn.revision.lock().unwrap_or_else(|p| p.into_inner());
            *applied = (*applied).max(r);
        }
    };
    let owned = |tx: &crate::store::sqlite::SqliteTx, state: PublicationState| -> anyhow::Result<bool> {
        Ok(tx.publication(alias, &agent)?.is_some_and(|p| p.state == state))
    };
    match (op.kind, outcome) {
        (kind, OpOutcome::Pending(why)) => {
            tracing::info!(board = alias, op = op.op_id, "board op pending: {why}");
            if let Some(done) = core.local_tx(|tx| stored(tx)).map_err(internal)? {
                return done;
            }
            Ok(match kind {
                BoardOpKind::Publish => "publishing",
                BoardOpKind::Unpublish => "unpublishing",
                BoardOpKind::Migrate => "pending",
            }
            .into())
        }
        (BoardOpKind::Migrate, OpOutcome::Done(result, _)) => {
            if let Some(done) = core.local_tx(|tx| stored(tx)).map_err(internal)? {
                return done;
            }
            super::publish::apply_manifest(core, conn, op, &result)
        }
        (BoardOpKind::Publish, OpOutcome::Done(result, seq)) => {
            let published: Option<Published> = serde_json::from_value(result.clone()).ok();
            let duplicate = core
                .local_tx(|tx| {
                    if let Some(done) = stored(tx)? {
                        return Ok(Some(done));
                    }
                    if let Some(p) = &published
                        && owned(tx, PublicationState::Publishing)?
                    {
                        tx.update_publication(alias, &agent, Some((&p.mailbox_id, p.route_generation)), PublicationState::Published, &now)?;
                        // Only the completion that published the row marks it, and before the
                        // commit, under the store lock reconciliation reads the marks under:
                        // an older snapshot never sees the row published without the mark, and a
                        // duplicate never moves the mark past a newer withdrawal.
                        if let Some(seq) = seq {
                            let mut done = conn.completions.lock().unwrap_or_else(|p| p.into_inner());
                            let mark = done.entry(agent.clone()).or_default();
                            *mark = (*mark).max(seq);
                        }
                        advance(p.revision);
                    }
                    tx.finish_op(&op.op_id, BoardOpState::Done, Some(&result), &now)?;
                    Ok(None)
                })
                .map_err(internal)?;
            if let Some(done) = duplicate {
                return done;
            }
            Ok("published".into())
        }
        (BoardOpKind::Unpublish, OpOutcome::Done(result, _)) => {
            let mailbox = op.request["mailbox_id"].as_str().unwrap_or_default().to_string();
            let duplicate = core
                .local_tx(|tx| {
                    if let Some(done) = stored(tx)? {
                        return Ok(Some(done));
                    }
                    if tx.publication(alias, &agent)?.is_some_and(|p| p.state == PublicationState::Unpublishing && p.mailbox_id.as_deref() == Some(mailbox.as_str())) {
                        tx.delete_publication(alias, &agent)?;
                        advance(result["revision"].as_u64());
                    }
                    tx.finish_op(&op.op_id, BoardOpState::Done, Some(&result), &now)?;
                    Ok(None)
                })
                .map_err(internal)?;
            match duplicate {
                Some(done) => done,
                None => Ok("unpublished".into()),
            }
        }
        (kind, OpOutcome::Refused(code, message)) => {
            let moved = kind == BoardOpKind::Unpublish && matches!(code, ErrorCode::Conflict | ErrorCode::NotFound);
            let mailbox = op.request["mailbox_id"].as_str().unwrap_or_default().to_string();
            let generation = op.request["expected_generation"].as_u64().unwrap_or_default();
            let duplicate = core
                .local_tx(|tx| {
                    if let Some(done) = stored(tx)? {
                        return Ok(Some(done));
                    }
                    let error = json!({"error": {"code": code, "message": message}});
                    match kind {
                        BoardOpKind::Publish => {
                            if owned(tx, PublicationState::Publishing)? {
                                tx.delete_publication(alias, &agent)?;
                            }
                            tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&error), &now)?;
                        }
                        // The mailbox moved (or is gone) meanwhile: the local row goes, the new
                        // route stays.
                        BoardOpKind::Unpublish if moved => {
                            if owned(tx, PublicationState::Unpublishing)? {
                                tx.delete_publication(alias, &agent)?;
                            }
                            tx.finish_op(&op.op_id, BoardOpState::Done, Some(&json!({"moved": true, "message": message})), &now)?;
                        }
                        BoardOpKind::Unpublish => {
                            if owned(tx, PublicationState::Unpublishing)? {
                                tx.update_publication(alias, &agent, Some((&mailbox, generation)), PublicationState::Published, &now)?;
                            }
                            tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&error), &now)?;
                        }
                        BoardOpKind::Migrate => {
                            tx.finish_op(&op.op_id, BoardOpState::Failed, Some(&error), &now)?;
                        }
                    }
                    Ok(None)
                })
                .map_err(internal)?;
            match duplicate {
                Some(done) => done,
                None if moved => Ok("unpublished".into()),
                None => Err((code, format!("{alias}: {message}"))),
            }
        }
    }
}

/// Resumes the board's pending migrate op (over HTTPS), then connects when it is done.
pub async fn resume_migration(core: Arc<Core>, conn: Arc<BoardConn>, op: BoardOp) {
    let mut delay = Duration::from_secs(1);
    loop {
        if conn.forgotten.load(Ordering::SeqCst) {
            return;
        }
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

/// `board.info` over HTTPS (no hello).
pub async fn fetch_info(conn: &BoardConn, wait: Duration) -> Result<BoardInfo, String> {
    let resp = rpc(conn, json!({"op": op::BOARD_INFO, "body": {}}), wait).await?;
    match (resp.result, resp.error) {
        (Some(r), None) => serde_json::from_value(r).map_err(|e| format!("{}: board.info: {e}", conn.alias)),
        (_, Some(e)) => Err(format!("{}: {}", conn.alias, e.message)),
        _ => Err(format!("{}: board.info: empty response", conn.alias)),
    }
}

/// The board says this host is migrated, but no local migrate op is done (the local record was
/// lost, or the alias was forgotten and enrolled again): record it done and connect.
pub fn adopt_migrated(core: &Arc<Core>, conn: &Arc<BoardConn>) -> anyhow::Result<()> {
    if conn.status() != Status::NotMigrated {
        return Ok(());
    }
    let now = iso_from_ms(now_ms());
    core.local_tx(|tx| {
        if tx.migrate_op(&conn.alias)?.is_none() {
            tx.insert_op(&BoardOp {
                op_id: crate::util::new_ulid(),
                board: conn.alias.clone(),
                kind: BoardOpKind::Migrate,
                agent_id: None,
                request: json!({"publish": []}),
                state: BoardOpState::Done,
                result: Some(json!({"adopted": "board.info: mailbox_only"})),
                created_at: now.clone(),
                updated_at: now.clone(),
            })?;
        }
        Ok(())
    })?;
    tracing::info!(board = conn.alias, "the board reports this host migrated: connecting");
    start(core, conn);
    Ok(())
}

/// A board not migrated here: ask it (over HTTPS) whether it already counts this host as
/// migrated, until it answers.
pub async fn probe_migrated(core: Weak<Core>, conn: Arc<BoardConn>) {
    let mut delay = Duration::from_secs(1);
    // `conde setup` usually migrates right after a (re)start; ask only if it did not.
    tokio::time::sleep(Duration::from_secs(2)).await;
    loop {
        let Some(c) = core.upgrade() else { return };
        if conn.status() != Status::NotMigrated || conn.forgotten.load(Ordering::SeqCst) {
            return;
        }
        let wait = c.mux.tuning.call_timeout;
        drop(c);
        match fetch_info(&conn, wait).await {
            Ok(info) => {
                if info.mailbox_only
                    && let Some(c) = core.upgrade()
                    && let Err(e) = adopt_migrated(&c, &conn)
                {
                    tracing::error!(board = conn.alias, "cannot record the migration: {e:#}");
                }
                return;
            }
            Err(why) => tracing::debug!(board = conn.alias, "board.info: {why}"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::local::Publication;

    fn op(id: &str, kind: BoardOpKind, agent: &str, request: Value) -> BoardOp {
        BoardOp {
            op_id: id.into(),
            board: "dakai".into(),
            kind,
            agent_id: Some(agent.into()),
            request,
            state: BoardOpState::Pending,
            result: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    fn setup() -> (tempfile::TempDir, tokio::runtime::Runtime, Arc<Core>, Arc<BoardConn>, String) {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let cfg = BoardConfig { server: "https://b.example".into(), host_id: "homepc".into(), owner: "krisz".into(), key: "k".into() };
        let core = Core::for_tests(dir.path(), rt.handle().clone(), vec![BoardConn::new("dakai", Ok(cfg))]);
        let conn = core.board("dakai").unwrap();
        let agent = crate::util::new_ulid();
        core.local.with_tx(|tx| Ok(tx.insert_agent(&agent, core.principal.user_id, "worker")?)).unwrap();
        (dir, rt, core, conn, agent)
    }

    fn state(core: &Core, agent: &str) -> Option<PublicationState> {
        core.local_tx(|tx| Ok(tx.publication("dakai", agent)?)).unwrap().map(|p| p.state)
    }

    fn begin(core: &Core, o: &BoardOp, row: Option<Publication>, update: Option<(&str, u64, PublicationState)>) {
        core.local_tx(|tx| {
            tx.insert_op(o)?;
            if let Some(p) = &row {
                tx.insert_publication(p)?;
            }
            if let Some((mailbox, generation, s)) = update {
                tx.update_publication("dakai", o.agent_id.as_deref().unwrap(), Some((mailbox, generation)), s, "t")?;
            }
            Ok(())
        })
        .unwrap();
    }

    fn publishing(agent: &str) -> Publication {
        Publication {
            board: "dakai".into(),
            agent_id: agent.into(),
            shared_name: "worker".into(),
            mailbox_id: None,
            route_generation: None,
            state: PublicationState::Publishing,
            updated_at: "t".into(),
        }
    }

    #[test]
    fn a_repeated_publish_completion_leaves_a_newer_unpublish_alone() {
        let (_dir, _rt, core, conn, agent) = setup();
        let answer = json!({"name": "worker", "mailbox_id": "01MB", "route_generation": 2});
        let publish = op("01P", BoardOpKind::Publish, &agent, json!({"name": "worker"}));
        begin(&core, &publish, Some(publishing(&agent)), None);
        assert_eq!(finish_op(&core, &conn, &publish, OpOutcome::Done(answer.clone(), Some(5))), Ok("published".into()));
        assert_eq!(state(&core, &agent), Some(PublicationState::Published));
        // a newer unpublish is in flight
        let unpublish = op("01U", BoardOpKind::Unpublish, &agent, json!({"mailbox_id": "01MB", "expected_generation": 2}));
        begin(&core, &unpublish, None, Some(("01MB", 2, PublicationState::Unpublishing)));
        // the reconnect worker finishes the old publish again
        assert_eq!(finish_op(&core, &conn, &publish, OpOutcome::Done(answer, Some(9))), Ok("published".into()));
        assert_eq!(state(&core, &agent), Some(PublicationState::Unpublishing), "admission stays closed");
    }

    #[test]
    fn a_repeated_unpublish_completion_leaves_a_newer_publish_alone() {
        let (_dir, _rt, core, conn, agent) = setup();
        let publish = op("01P", BoardOpKind::Publish, &agent, json!({"name": "worker"}));
        begin(&core, &publish, Some(publishing(&agent)), None);
        finish_op(&core, &conn, &publish, OpOutcome::Done(json!({"name": "worker", "mailbox_id": "01MB", "route_generation": 2}), Some(1))).unwrap();
        let unpublish = op("01U", BoardOpKind::Unpublish, &agent, json!({"mailbox_id": "01MB", "expected_generation": 2}));
        begin(&core, &unpublish, None, Some(("01MB", 2, PublicationState::Unpublishing)));
        let gone = json!({"mailbox_id": "01MB", "route_generation": 3});
        assert_eq!(finish_op(&core, &conn, &unpublish, OpOutcome::Done(gone.clone(), Some(2))), Ok("unpublished".into()));
        assert_eq!(state(&core, &agent), None);
        // published again, still in flight
        let again = op("01Q", BoardOpKind::Publish, &agent, json!({"name": "worker"}));
        begin(&core, &again, Some(publishing(&agent)), None);
        // the reconnect worker finishes the old unpublish again
        assert_eq!(finish_op(&core, &conn, &unpublish, OpOutcome::Done(gone, Some(3))), Ok("unpublished".into()));
        assert_eq!(state(&core, &agent), Some(PublicationState::Publishing), "the newer publication stays");
    }

    // Desired invariant: a duplicate response does not make a newer withdrawal snapshot stale.
    #[test]
    fn review_duplicate_completion_does_not_mask_a_newer_routes_withdrawal() {
        let (_dir, _rt, core, conn, agent) = setup();
        let answer = json!({"name": "worker", "mailbox_id": "01MB", "route_generation": 2});
        let publish = op("01P", BoardOpKind::Publish, &agent, json!({"name": "worker"}));
        begin(&core, &publish, Some(publishing(&agent)), None);
        finish_op(&core, &conn, &publish, OpOutcome::Done(answer.clone(), Some(5))).unwrap();
        assert_eq!(state(&core, &agent), Some(PublicationState::Published));

        // The server's empty routes snapshot (frame 8) reflects a takeover after publishing.
        // Its callback is queued while the idempotent response for the old publish (frame 9)
        // reaches the concurrently resumed op worker. The operation is already locally Done.
        finish_op(&core, &conn, &publish, OpOutcome::Done(answer, Some(9))).unwrap();
        reconcile(&core, "dakai", &[], 8, None);
        assert_eq!(state(&core, &agent), None,
            "a duplicate completion must not suppress a newer routes withdrawal");
    }

    // Models an actual host.migrate_v2 response arriving after board.forget completed.
    #[test]
    fn late_migration_completion_must_not_recreate_forgotten_publications() {
        let (_dir, _rt, core, conn, agent) = setup();
        let mut migration = op("01LATEFORGOTTENMIGRATION", BoardOpKind::Migrate, &agent, json!({"publish": ["01REMOTEAGENT"]}));
        migration.agent_id = None;
        begin(&core, &migration, None, None);
        let answer = json!({"bindings": [{
            "agent_id": "01REMOTEAGENT", "name": "worker",
            "address": format!("codex:local-thread@{}", core.principal.host),
            "pin": null,
            "published": {"mailbox_id": "01REMOTEMAILBOX", "route_generation": 2}
        }]});
        let mut req = crate::protocol::Request::new("forget", crate::surface::BOARD_FORGET, json!({}));
        req.actor = Some(format!("human:tester@{}", core.principal.host));
        req.board = Some("dakai".into());
        let forgotten = super::super::publish::forget(&core, &req).unwrap();
        assert_eq!(forgotten["ops_removed"], 1);
        assert!(core.board("dakai").is_none());
        assert!(core.local_tx(|tx| Ok(tx.op(&migration.op_id)?)).unwrap().is_none());
        assert_eq!(state(&core, &agent), None);
        // The real HTTP request or startup resume worker still owns this Arc and op.
        let _late = finish_op(&core, &conn, &migration, OpOutcome::Done(answer, None));
        assert_eq!(state(&core, &agent), None, "board.forget must prevent its outstanding migration from recreating exposure");
    }
}
