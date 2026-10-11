//! The delivery multiplexer (DESIGN-v2.md §5): one thread owns the notifier (and so the one
//! Claude reply socket) and serves every origin, the local board and each shared board, from
//! its own bounded queue, round-robin.
//!
//! - The thread never waits on a remote call: a board's jobs arrive through its client's
//!   callback, its outcomes leave through a per-board map that the board's task drains into
//!   the client's book, and routes and acks are handled off this thread.
//! - Each origin has a receipt budget R: one slot is reserved before an injection and held,
//!   with the token-map entry and the notifier's mapping, until the attempt resolves (its
//!   final outcome was accepted by the origin's book, or the local board recorded it).
//! - Every job gets a locally minted token; the harness sees only the token, the board only
//!   its own attempt id.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use conde_remote::{BridgeClient, Endpoint, Notify, NotifyFields, NotifyKind, OutcomeReason, OutcomeStatus};
use serde_json::{Value, json};

use crate::address::{Address, Kind};
use crate::dispatch::{Dispatcher, Principal};
use crate::notify::{Notifier, NotifyOutcome, ReceiptStatus};
use crate::protocol::Request;
use crate::protocol::v2::LOCAL_BOARD;
use crate::store::Tx;
use crate::store::local::{BoardOpKind, LocalTx, PublicationState};
use crate::util::new_ulid;

/// Jobs injected per turn at most, before receipts are polled again.
const MAX_PER_TURN: usize = 16;
/// Delivery failures kept for the local doctor.
const FAILURES_KEPT: usize = 20;

/// The multiplexer's bounds and cadences.
#[derive(Clone, Debug)]
pub struct Tuning {
    /// The admission window W each board connection declares (its queue's capacity).
    pub window: u32,
    /// The receipt budget R of each origin.
    pub receipt_budget: usize,
    /// The local origin's queue capacity (its claim limit).
    pub local_queue: usize,
    /// Endpoint reporting, session-replacement and liveness checks.
    pub rescan: Duration,
    /// How long an aggregate view waits for the boards.
    pub aggregate_wait: Duration,
    /// How long a request resolved to one shared board may take.
    pub call_timeout: Duration,
    /// The board clients' retained-outcome budget (`None`: the client's default).
    pub max_unacked_outcomes: Option<usize>,
    /// The board clients' reconnect backoff ceiling (`None`: the client's default).
    pub backoff_max: Option<Duration>,
    /// The board clients' idle timeout (`None`: the client's default).
    pub idle_timeout: Option<Duration>,
    /// The board clients' outcome ack timeout (`None`: the client's default).
    pub outcome_ack_timeout: Option<Duration>,
    /// The board clients' delay before resending an outcome the board asked to retry
    /// (`None`: the client's default backoff).
    pub outcome_resend: Option<Duration>,
    /// Tests only: how long the routes callback waits before applying a snapshot.
    #[doc(hidden)]
    pub routes_delay: Option<Duration>,
    /// Tests only: how long a board reload waits between deciding to replace the connection
    /// and replacing it.
    #[doc(hidden)]
    pub reload_delay: Option<Duration>,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            window: conde_remote::DEFAULT_WINDOW,
            receipt_budget: 256,
            local_queue: conde_remote::DEFAULT_WINDOW as usize,
            rescan: Duration::from_secs(5),
            aggregate_wait: Duration::from_secs(2),
            call_timeout: Duration::from_secs(30),
            max_unacked_outcomes: None,
            backoff_max: None,
            idle_timeout: None,
            outcome_ack_timeout: None,
            outcome_resend: None,
            routes_delay: None,
            reload_delay: None,
        }
    }
}

/// A job of the local origin, as the local board's `bridge.claim` hands it out.
struct LocalJob {
    attempt_id: String,
    address: Address,
    text: String,
    /// `owner/name` of the recipient on the local board.
    recipient: String,
    binding_version: i64,
}

enum Job {
    Local(LocalJob),
    Board(Box<Notify>),
}

impl Job {
    fn attempt_id(&self) -> &str {
        match self {
            Job::Local(j) => &j.attempt_id,
            Job::Board(n) => &n.attempt_id,
        }
    }
}

/// Who the attempt went to on the local registry: an agent id (a board's job) or the
/// recipient's `owner/name` (a local job).
#[derive(Clone, Debug)]
enum AgentRef {
    Id(String),
    Name(String),
}

/// One injected, unresolved attempt: what a late receipt is routed by.
struct Entry {
    origin: String,
    attempt_id: String,
    agent: AgentRef,
    /// The local agent's `binding_version` at injection (the local incarnation; never sent).
    incarnation: i64,
    address: Address,
    status: OutcomeStatus,
    /// A final outcome is recorded; later receipts are ignored.
    resolved: bool,
}

/// An outcome waiting to be accepted by a board client's book.
#[derive(Clone, Debug)]
pub(crate) struct OutcomeRecord {
    pub token: String,
    pub attempt_id: String,
    pub status: OutcomeStatus,
    pub reason: Option<OutcomeReason>,
    /// Nothing is reported for the attempt after this one: accepting it resolves it.
    pub last: bool,
    pub rev: u64,
}

struct Origin {
    jobs: VecDeque<Job>,
    cap: usize,
    used: usize,
    budget: usize,
    /// Outcomes for the board's book, by token: the latest wins, a terminal one is never
    /// replaced by a non-terminal one.
    outcomes: HashMap<String, OutcomeRecord>,
    /// Attempt ids held by this origin (queued, injected, or settled and not yet accepted):
    /// a notify for one of them is ignored.
    held: HashSet<String>,
    /// The connection's routes are applied since its last connect (boards).
    reconciled: bool,
    stopped: Option<String>,
    client: Option<BridgeClient>,
}

impl Origin {
    fn new(cap: usize, budget: usize, reconciled: bool) -> Origin {
        Origin {
            jobs: VecDeque::new(),
            cap,
            used: 0,
            budget,
            outcomes: HashMap::new(),
            held: HashSet::new(),
            reconciled,
            stopped: None,
            client: None,
        }
    }
}

#[derive(Default)]
struct State {
    origins: BTreeMap<String, Origin>,
    entries: HashMap<String, Entry>,
    /// Released tokens whose notifier mapping the delivery thread drops next.
    forget: Vec<String>,
    rev: u64,
    failures: VecDeque<String>,
    /// Round-robin start.
    turn: usize,
}

/// One origin's counters, for `doctor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginStats {
    pub origin: String,
    pub used: usize,
    pub budget: usize,
    pub queued: usize,
    pub cap: usize,
    pub outcomes: usize,
    pub stopped: Option<String>,
}

/// The multiplexer's shared state.
pub struct Mux {
    state: Mutex<State>,
    wake: Condvar,
    pub tuning: Tuning,
}

impl Mux {
    pub fn new(tuning: Tuning) -> Mux {
        let mut state = State::default();
        state.origins.insert(LOCAL_BOARD.into(), Origin::new(tuning.local_queue, tuning.receipt_budget, true));
        Mux { state: Mutex::new(state), wake: Condvar::new(), tuning }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn notify_all(&self) {
        self.wake.notify_all();
    }

    /// Adds a board origin (its connection starts unreconciled).
    pub fn add_board(&self, alias: &str, client: BridgeClient) {
        let mut st = self.lock();
        let mut origin = Origin::new(self.tuning.window as usize, self.tuning.receipt_budget, false);
        origin.client = Some(client);
        st.origins.insert(alias.to_string(), origin);
    }

    /// A new connection: its jobs wait until its routes are applied.
    pub fn connected(&self, alias: &str) {
        if let Some(o) = self.lock().origins.get_mut(alias) {
            o.reconciled = false;
        }
    }

    /// The connection's routes are applied: its jobs may be injected again.
    pub fn reconciled(&self, alias: &str) {
        if let Some(o) = self.lock().origins.get_mut(alias) {
            o.reconciled = true;
        }
        self.notify_all();
    }

    pub fn is_reconciled(&self, alias: &str) -> bool {
        self.lock().origins.get(alias).is_some_and(|o| o.reconciled)
    }

    /// A notify from the board's client (its callback thread; never blocks). A known attempt
    /// is ignored; one beyond the window is settled `failed` (the board retries it).
    pub fn push(&self, alias: &str, n: Notify) {
        let mut st = self.lock();
        let rev = next_rev(&mut st);
        let Some(o) = st.origins.get_mut(alias) else { return };
        if o.stopped.is_some() || o.held.contains(&n.attempt_id) {
            return;
        }
        o.held.insert(n.attempt_id.clone());
        if o.jobs.len() >= o.cap {
            tracing::warn!(board = alias, "notify beyond the admission window; settled failed for a retry");
            let token = new_ulid();
            o.outcomes.insert(
                token.clone(),
                OutcomeRecord {
                    token,
                    attempt_id: n.attempt_id,
                    status: OutcomeStatus::Failed,
                    reason: Some(OutcomeReason::TransportError),
                    last: true,
                    rev,
                },
            );
            return;
        }
        o.jobs.push_back(Job::Board(Box::new(n)));
        drop(st);
        self.notify_all();
    }

    /// The board's outcomes in record order, for its book.
    pub(crate) fn outcomes(&self, alias: &str) -> Vec<OutcomeRecord> {
        let st = self.lock();
        let mut out: Vec<OutcomeRecord> =
            st.origins.get(alias).map(|o| o.outcomes.values().cloned().collect()).unwrap_or_default();
        out.sort_by_key(|r| r.rev);
        out
    }

    /// The board's book accepted `r`: drop it from the map unless a newer outcome replaced it,
    /// and resolve the attempt when it was the last.
    pub(crate) fn accepted(&self, alias: &str, r: &OutcomeRecord) {
        let mut st = self.lock();
        let Some(o) = st.origins.get_mut(alias) else { return };
        if o.outcomes.get(&r.token).is_some_and(|cur| cur.rev == r.rev) {
            o.outcomes.remove(&r.token);
            if r.last {
                o.held.remove(&r.attempt_id);
                release(&mut st, &r.token);
            }
        }
        drop(st);
        self.notify_all();
    }

    /// The board's connection stopped for good: its jobs, outcomes and slots are dropped (the
    /// board re-attempts them through its next instance).
    pub fn stop_board(&self, alias: &str, why: &str) {
        self.stop_if(alias, why, |_| true);
    }

    /// [`stop_board`](Self::stop_board) when `client` is still the board's connection: a
    /// reloaded board's new connection is never stopped by its predecessor.
    pub fn stop_client(&self, alias: &str, client: &BridgeClient, why: &str) {
        self.stop_if(alias, why, |o| {
            o.and_then(|o| o.client.as_ref()).is_some_and(|c| c.bridge_instance() == client.bridge_instance())
        });
    }

    fn stop_if(&self, alias: &str, why: &str, current: impl Fn(Option<&Origin>) -> bool) {
        let mut st = self.lock();
        if !current(st.origins.get(alias)) {
            return;
        }
        let tokens: Vec<String> =
            st.entries.iter().filter(|(_, e)| e.origin == alias).map(|(t, _)| t.clone()).collect();
        for t in tokens {
            release(&mut st, &t);
        }
        if let Some(o) = st.origins.get_mut(alias) {
            o.stopped = Some(why.to_string());
            o.jobs.clear();
            o.outcomes.clear();
            o.held.clear();
            o.used = 0;
        }
        drop(st);
        self.notify_all();
    }

    /// A forgotten board: its origin goes, with its jobs, outcomes and slots.
    pub fn remove_board(&self, alias: &str) {
        self.stop_board(alias, "forgotten");
        self.lock().origins.remove(alias);
        self.notify_all();
    }

    pub fn stats(&self) -> Vec<OriginStats> {
        let st = self.lock();
        st.origins
            .iter()
            .map(|(name, o)| OriginStats {
                origin: name.clone(),
                used: o.used,
                budget: o.budget,
                queued: o.jobs.len(),
                cap: o.cap,
                outcomes: o.outcomes.len(),
                stopped: o.stopped.clone(),
            })
            .collect()
    }

    /// Recent delivery failures with their local details (doctor only).
    pub fn failures(&self) -> Vec<String> {
        self.lock().failures.iter().cloned().collect()
    }

    fn fail(&self, detail: String) {
        tracing::warn!("delivery failure: {detail}");
        let mut st = self.lock();
        st.failures.push_back(detail);
        while st.failures.len() > FAILURES_KEPT {
            st.failures.pop_front();
        }
    }

    /// Records a board outcome for the book (the latest wins; terminal over non-terminal).
    fn record(&self, alias: &str, token: &str, attempt_id: &str, status: OutcomeStatus, reason: Option<OutcomeReason>, last: bool) {
        let mut st = self.lock();
        let rev = next_rev(&mut st);
        if last && let Some(e) = st.entries.get_mut(token) {
            e.resolved = true;
        }
        if let Some(e) = st.entries.get_mut(token) {
            e.status = status;
        }
        let Some(o) = st.origins.get_mut(alias) else { return };
        if o.stopped.is_some() {
            return;
        }
        if let Some(cur) = o.outcomes.get(token)
            && cur.status.is_terminal()
            && !status.is_terminal()
        {
            return;
        }
        o.outcomes.insert(
            token.to_string(),
            OutcomeRecord { token: token.to_string(), attempt_id: attempt_id.to_string(), status, reason, last, rev },
        );
    }

    fn release_now(&self, token: &str) {
        let mut st = self.lock();
        release(&mut st, token);
        drop(st);
        self.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let st = self.lock();
        let _ = self.wake.wait_timeout(st, timeout);
    }
}

fn next_rev(st: &mut State) -> u64 {
    st.rev += 1;
    st.rev
}

/// Releases the slot, the token-map entry and (next turn) the notifier's mapping.
fn release(st: &mut State, token: &str) {
    if let Some(e) = st.entries.remove(token) {
        if let Some(o) = st.origins.get_mut(&e.origin) {
            o.used = o.used.saturating_sub(1);
            o.held.remove(&e.attempt_id);
        }
        st.forget.push(token.to_string());
    }
}

/// The notification text of an admitted v2 notify, rendered from its structured fields with
/// the board's alias; `text` of the frame is never read.
pub fn render(alias: &str, n: &Notify, f: &NotifyFields) -> String {
    let id = format!("{alias}:{}", n.message_id);
    let place = &f.place;
    let mut text = match f.kind {
        NotifyKind::Dm => format!("conde [{alias}]: new message {id} from {}. Read it with: conde read {id}", f.sender),
        NotifyKind::Mention => format!(
            "conde [{alias}]: you were mentioned in {place} by {} (message {id}). Read it with: conde read '{place}' --board {alias}{}",
            f.sender,
            if f.member { String::new() } else { format!(" after joining: conde join '{place}' --board {alias}") }
        ),
        NotifyKind::Post => format!(
            "conde [{alias}]: {} posted in {place} (message {id}). Read it with: conde read '{place}' --board {alias}",
            f.sender
        ),
    };
    if !f.badges.is_empty() {
        let badges: Vec<String> = f.badges.iter().map(|b| format!("{} {}", b.place, b.unread)).collect();
        text.push_str(&format!(" | Also unread on {alias}: {}", badges.join(", ")));
    }
    text
}

/// What admission decided for a board's job, at the moment of injection.
enum Admission {
    Inject { agent_id: String, incarnation: i64, address: Address, text: String },
    Settle(OutcomeStatus, OutcomeReason),
}

/// The delivery thread's view of the local board.
pub struct Delivery {
    pub mux: Arc<Mux>,
    pub local: Arc<Dispatcher>,
    /// The local board's bridge principal (its own instance).
    pub bridge: Principal,
    pub interval: Duration,
    /// Local outcomes not recorded yet, in order; each is retried until the local board
    /// records it, and its reservation is released only then.
    pub pending: Mutex<VecDeque<LocalOutcome>>,
}

/// An outcome for the local board, kept until it is recorded.
#[derive(Clone, Debug)]
pub struct LocalOutcome {
    token: String,
    attempt_id: String,
    status: String,
    reason: Option<String>,
    /// The attempt resolves when this is recorded: release its slot and mapping.
    release: bool,
}

/// What recording a local outcome came to.
enum Recording {
    Recorded,
    /// The local board refused it for good (not this bridge's attempt, bad input).
    Refused,
    /// Storage did not take it; tried again later.
    Retry,
}

impl Delivery {
    fn local_call(&self, op: &str, body: Value) -> anyhow::Result<Value> {
        let resp = self.local.dispatch(&self.bridge, &Request::new(new_ulid(), op, body));
        match (resp.result, resp.error) {
            (Some(r), None) => Ok(r),
            (_, Some(e)) => Err(anyhow::anyhow!("{op}: {}", e.message)),
            _ => Err(anyhow::anyhow!("{op}: empty response")),
        }
    }

    /// Records a local attempt's outcome on the local board (v1 statuses and details).
    fn local_report(&self, attempt_id: &str, outcome: &str, reason: Option<&str>) -> Recording {
        let body = json!({"attempt_id": attempt_id, "outcome": outcome, "reason": reason});
        let resp = self.local.dispatch(&self.bridge, &Request::new(new_ulid(), "bridge.report", body));
        match (resp.result, resp.error) {
            (Some(r), None) => {
                match r.get("ignored").and_then(Value::as_str) {
                    Some(why) => tracing::info!(attempt = attempt_id, outcome, "receipt ignored: {why}"),
                    None => tracing::info!(attempt = attempt_id, outcome, reason = reason.unwrap_or_default(), "attempt settled"),
                }
                Recording::Recorded
            }
            (_, Some(e)) if e.code == crate::protocol::ErrorCode::Internal => {
                tracing::warn!(attempt = attempt_id, outcome, "outcome not recorded yet: {}", e.message);
                Recording::Retry
            }
            (_, Some(e)) => {
                tracing::warn!(attempt = attempt_id, outcome, "report refused: {}", e.message);
                Recording::Refused
            }
            _ => Recording::Retry,
        }
    }

    /// Queues a local outcome; a releasing one resolves its attempt now (later receipts are
    /// ignored) and frees the slot once recorded.
    fn queue_local(&self, token: &str, attempt_id: &str, status: &str, reason: Option<&str>, release: bool) {
        if release && let Some(e) = self.mux.lock().entries.get_mut(token) {
            e.resolved = true;
        }
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).push_back(LocalOutcome {
            token: token.to_string(),
            attempt_id: attempt_id.to_string(),
            status: status.to_string(),
            reason: reason.map(str::to_string),
            release,
        });
        self.flush_local();
    }

    /// Records the queued local outcomes in order; stops at the first one storage did not take.
    fn flush_local(&self) {
        loop {
            let Some(next) = self.pending.lock().unwrap_or_else(|p| p.into_inner()).front().cloned() else { return };
            match self.local_report(&next.attempt_id, &next.status, next.reason.as_deref()) {
                Recording::Retry => return,
                Recording::Recorded | Recording::Refused => {
                    self.pending.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
                    if next.release {
                        self.mux.release_now(&next.token);
                    }
                }
            }
        }
    }

    /// Claims up to the local queue's free capacity.
    fn refill_local(&self) {
        let room = {
            let st = self.mux.lock();
            let o = &st.origins[LOCAL_BOARD];
            o.cap.saturating_sub(o.jobs.len())
        };
        if room == 0 {
            return;
        }
        let claim = match self.local_call("bridge.claim", json!({"limit": room})) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("local claim failed: {e:#}");
                return;
            }
        };
        let mut st = self.mux.lock();
        let o = st.origins.get_mut(LOCAL_BOARD).expect("local origin");
        for job in claim["jobs"].as_array().into_iter().flatten() {
            let attempt_id = job["attempt_id"].as_str().unwrap_or_default().to_string();
            let Ok(address) = Address::parse(job["address"].as_str().unwrap_or_default(), &self.bridge.host) else {
                continue;
            };
            o.held.insert(attempt_id.clone());
            o.jobs.push_back(Job::Local(LocalJob {
                attempt_id,
                address,
                text: job["text"].as_str().unwrap_or_default().to_string(),
                recipient: job["recipient"].as_str().unwrap_or_default().to_string(),
                binding_version: job["binding_version"].as_i64().unwrap_or_default(),
            }));
        }
    }

    /// The next origin (round-robin) that may inject, with its next job.
    fn next_job(&self, skip: &HashSet<String>) -> Option<(String, Job)> {
        let mut st = self.mux.lock();
        let names: Vec<String> = st.origins.keys().cloned().collect();
        let n = names.len();
        for i in 0..n {
            let name = &names[(st.turn + i) % n];
            if skip.contains(name) {
                continue;
            }
            let o = st.origins.get_mut(name).expect("origin");
            if o.stopped.is_some() || !o.reconciled || o.used >= o.budget || o.jobs.is_empty() {
                continue;
            }
            let job = o.jobs.pop_front().expect("job");
            st.turn = (st.turn + i + 1) % n;
            return Some((name.clone(), job));
        }
        None
    }

    fn admit<N: Notifier>(&self, alias: &str, n: &Notify, notifier: &mut N) -> Admission {
        let unavailable = Admission::Settle(OutcomeStatus::Refused, OutcomeReason::RecipientUnavailable);
        let Endpoint::Mailbox { mailbox_id } = &n.endpoint else { return unavailable };
        let Some(fields) = n.structured() else { return unavailable };
        let Some(sqlite) = self.local.sqlite() else { return unavailable };
        let found = (|| {
            let tx = sqlite.begin_local()?;
            let publication = tx.publication_by_mailbox(alias, mailbox_id)?;
            let publishing = tx.pending_ops(Some(alias))?.iter().any(|op| op.kind == BoardOpKind::Publish);
            let agent = match &publication {
                Some(p) => tx.agent_by_id(&p.agent_id)?,
                None => None,
            };
            Ok::<_, crate::store::StoreError>((publication, publishing, agent))
        })();
        let (publication, publishing, agent) = match found {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(board = alias, "admission lookup failed: {e}");
                return Admission::Settle(OutcomeStatus::Failed, OutcomeReason::TransportError);
            }
        };
        let Some(p) = publication else {
            // A publish whose answer is not stored yet may be this mailbox: retried, not refused.
            return if publishing {
                Admission::Settle(OutcomeStatus::Failed, OutcomeReason::RecipientUnavailable)
            } else {
                unavailable
            };
        };
        let generation = p.route_generation.unwrap_or_default();
        if p.state != PublicationState::Published || n.binding_version < generation {
            return unavailable;
        }
        if n.binding_version > generation {
            // The board routes a newer generation here than applied yet: retried after its routes.
            return Admission::Settle(OutcomeStatus::Failed, OutcomeReason::RecipientUnavailable);
        }
        let Some(agent) = agent else { return unavailable };
        let Some(address) = agent.address.clone() else {
            return Admission::Settle(OutcomeStatus::Failed, OutcomeReason::RecipientOffline);
        };
        if address.kind == Kind::Human || address.kind == Kind::Mailbox {
            return unavailable;
        }
        if !notifier.is_live(&address) {
            self.mux.fail(format!("{alias}: {} ({address}) has no live session", agent.canonical()));
            return Admission::Settle(OutcomeStatus::Failed, OutcomeReason::RecipientOffline);
        }
        Admission::Inject {
            agent_id: agent.id,
            incarnation: agent.binding_version,
            address,
            text: render(alias, n, fields),
        }
    }

    /// Reserves a slot and records the entry; returns the token.
    fn reserve(&self, origin: &str, attempt_id: &str, agent: AgentRef, incarnation: i64, address: &Address) -> String {
        let token = new_ulid();
        let mut st = self.mux.lock();
        if let Some(o) = st.origins.get_mut(origin) {
            o.used += 1;
        }
        st.entries.insert(
            token.clone(),
            Entry {
                origin: origin.to_string(),
                attempt_id: attempt_id.to_string(),
                agent,
                incarnation,
                address: address.clone(),
                status: OutcomeStatus::Accepted,
                resolved: false,
            },
        );
        token
    }

    fn inject<N: Notifier>(&self, origin: &str, job: Job, notifier: &mut N) {
        match job {
            Job::Local(j) => {
                let token = self.reserve(origin, &j.attempt_id, AgentRef::Name(j.recipient.clone()), j.binding_version, &j.address);
                let result = notifier.notify(&j.address, &j.text, &token);
                let (status, reason) = result.report();
                // A receipt can follow only a Claude acceptance.
                let awaits_receipt = j.address.kind == Kind::Claude && result == NotifyOutcome::Accepted;
                self.queue_local(&token, &j.attempt_id, status, reason, !awaits_receipt);
            }
            Job::Board(n) => match self.admit(origin, &n, notifier) {
                Admission::Settle(status, reason) => {
                    let token = new_ulid();
                    self.mux.record(origin, &token, &n.attempt_id, status, Some(reason), true);
                }
                Admission::Inject { agent_id, incarnation, address, text } => {
                    let token = self.reserve(origin, &n.attempt_id, AgentRef::Id(agent_id), incarnation, &address);
                    let result = notifier.notify(&address, &text, &token);
                    let (status, reason, last) = match (&result, address.kind) {
                        // Claude's provisional acceptance: receipts follow.
                        (NotifyOutcome::Accepted, Kind::Claude) => (OutcomeStatus::Accepted, None, false),
                        // Codex: an accepted injection is the confirmed reach.
                        (NotifyOutcome::Accepted, _) => (OutcomeStatus::Delivered, None, true),
                        // Codex queued it for the thread's next turn; the board settles it on
                        // the read, so nothing more is reported from here.
                        (NotifyOutcome::Held, _) => (OutcomeStatus::Held, None, true),
                        (NotifyOutcome::Failed(detail), _) => {
                            self.mux.fail(format!("{origin}: delivery to {address} failed: {detail}"));
                            (OutcomeStatus::Failed, Some(OutcomeReason::TransportError), true)
                        }
                    };
                    self.mux.record(origin, &token, &n.attempt_id, status, reason, last);
                }
            },
        }
    }

    fn receipts<N: Notifier>(&self, notifier: &mut N) -> usize {
        let receipts = notifier.poll_receipts();
        let n = receipts.len();
        for r in receipts {
            let found = {
                let st = self.mux.lock();
                st.entries
                    .get(&r.attempt_id)
                    .filter(|e| !e.resolved)
                    .map(|e| (e.origin.clone(), e.attempt_id.clone()))
            };
            // An unknown token (released, or minted by an earlier process) is ignored.
            let Some((origin, attempt_id)) = found else {
                tracing::debug!(token = r.attempt_id, "receipt for an unknown or resolved token ignored");
                continue;
            };
            let terminal = !matches!(r.status, ReceiptStatus::Held);
            if origin == LOCAL_BOARD {
                let (status, reason) = r.status.report();
                if !terminal && let Some(e) = self.mux.lock().entries.get_mut(&r.attempt_id) {
                    e.status = OutcomeStatus::Held;
                }
                self.queue_local(&r.attempt_id, &attempt_id, status, reason, terminal);
                continue;
            }
            let (status, reason) = match &r.status {
                ReceiptStatus::Held => (OutcomeStatus::Held, None),
                ReceiptStatus::Delivered => (OutcomeStatus::Delivered, None),
                ReceiptStatus::Expired => (OutcomeStatus::Expired, None),
                ReceiptStatus::Denied => (OutcomeStatus::Denied, None),
                ReceiptStatus::Refused => (OutcomeStatus::Refused, None),
                ReceiptStatus::Dropped { reason } => {
                    if !reason.is_empty() && reason != "duplicate" {
                        self.mux.fail(format!("{origin}: attempt dropped by the harness: {reason}"));
                    }
                    (OutcomeStatus::Dropped, (reason == "duplicate").then_some(OutcomeReason::Duplicate))
                }
            };
            self.mux.record(&origin, &r.attempt_id, &attempt_id, status, reason, terminal);
        }
        n
    }

    /// Endpoint reporting, then session replacement and liveness of every unresolved attempt.
    fn rescan<N: Notifier>(&self, notifier: &mut N) {
        let Some(sqlite) = self.local.sqlite() else { return };
        let clients: Vec<(String, BridgeClient)> = {
            let st = self.mux.lock();
            st.origins
                .iter()
                .filter(|(_, o)| o.stopped.is_none())
                .filter_map(|(name, o)| o.client.clone().map(|c| (name.clone(), c)))
                .collect()
        };
        for (alias, client) in clients {
            let rows = (|| {
                let tx = sqlite.begin_local()?;
                let mut out = Vec::new();
                for p in tx.publications(Some(&alias))? {
                    if p.state != PublicationState::Published {
                        continue;
                    }
                    let address = tx.agent_by_id(&p.agent_id)?.and_then(|a| a.address);
                    if let (Some(mailbox_id), Some(address)) = (p.mailbox_id, address) {
                        out.push((mailbox_id, address));
                    }
                }
                Ok::<_, crate::store::StoreError>(out)
            })();
            match rows {
                Ok(rows) => {
                    let live: Vec<Endpoint> = rows
                        .into_iter()
                        .filter(|(_, a)| a.kind == Kind::Human || notifier.is_live(a))
                        .map(|(mailbox_id, _)| Endpoint::Mailbox { mailbox_id })
                        .collect();
                    client.set_endpoints(live);
                }
                Err(e) => tracing::warn!(board = alias, "endpoint scan failed: {e}"),
            }
        }

        let open: Vec<(String, String, AgentRef, i64, Address, OutcomeStatus, String)> = {
            let st = self.mux.lock();
            st.entries
                .iter()
                .filter(|(_, e)| !e.resolved)
                .map(|(t, e)| (t.clone(), e.origin.clone(), e.agent.clone(), e.incarnation, e.address.clone(), e.status, e.attempt_id.clone()))
                .collect()
        };
        for (token, origin, agent, incarnation, address, status, attempt_id) in open {
            let current = self.local.with_conn(|tx| match &agent {
                AgentRef::Id(id) => tx.agent_by_id(id),
                AgentRef::Name(canonical) => {
                    let name = canonical.split_once('/').map_or(canonical.as_str(), |(_, n)| n);
                    tx.agent_by_name(self.bridge.user_id, name)
                }
            });
            let current = match current {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("liveness lookup failed: {e}");
                    continue;
                }
            };
            let bound = current.as_ref().and_then(|a| a.address.clone().map(|addr| (a.binding_version, addr)));
            if origin == LOCAL_BOARD {
                // The local board abandons rebound attempts itself at its next claim.
                match bound {
                    Some((version, _)) if version == incarnation => {
                        if !notifier.is_live(&address) {
                            if status == OutcomeStatus::Held {
                                self.queue_local(&token, &attempt_id, "abandoned", Some("recipient session gone"), true);
                            } else {
                                self.mux.release_now(&token);
                            }
                        }
                    }
                    _ => self.mux.release_now(&token),
                }
                continue;
            }
            // The route went away (unpublished, taken, unbound): the board re-attempts it.
            let routed = match &agent {
                AgentRef::Id(id) => match sqlite.begin_local().and_then(|tx| tx.publication(&origin, id)) {
                    Ok(p) => p.is_some_and(|p| p.state == PublicationState::Published),
                    // A lookup that failed says nothing about the route: ask again next rescan.
                    Err(e) => {
                        tracing::warn!("route lookup failed: {e}");
                        continue;
                    }
                },
                AgentRef::Name(_) => true,
            };
            let reason = match bound {
                _ if !routed => Some(OutcomeReason::RecipientUnavailable),
                None => Some(OutcomeReason::RecipientOffline),
                Some((version, _)) if version != incarnation => Some(OutcomeReason::RecipientSessionReplaced),
                Some(_) if !notifier.is_live(&address) => Some(OutcomeReason::RecipientOffline),
                Some(_) => None,
            };
            if let Some(reason) = reason {
                tracing::info!(board = origin, reason = reason.as_str(), "unresolved attempt abandoned");
                self.mux.record(&origin, &token, &attempt_id, OutcomeStatus::Abandoned, Some(reason), true);
            }
        }
    }

    /// The delivery thread: runs until `stop`.
    pub fn run<N: Notifier>(self, mut notifier: N, stop: Arc<AtomicBool>) {
        let mut next_claim = Instant::now();
        let mut next_rescan = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let released: Vec<String> = std::mem::take(&mut self.mux.lock().forget);
            for token in released {
                notifier.forget(&token);
            }
            self.flush_local();
            let receipts = self.receipts(&mut notifier);
            if Instant::now() >= next_rescan {
                self.rescan(&mut notifier);
                next_rescan = Instant::now() + self.mux.tuning.rescan;
            }
            if Instant::now() >= next_claim {
                self.refill_local();
                next_claim = Instant::now() + self.interval;
            }
            let mut injected = 0;
            let mut skip = HashSet::new();
            while injected < MAX_PER_TURN && !stop.load(Ordering::Relaxed) {
                // One job per origin per round; a new round starts once every ready origin had one.
                let next = match self.next_job(&skip) {
                    None if !skip.is_empty() => {
                        skip.clear();
                        self.next_job(&skip)
                    }
                    next => next,
                };
                let Some((origin, job)) = next else { break };
                tracing::debug!(origin, attempt = job.attempt_id(), "injecting");
                self.inject(&origin, job, &mut notifier);
                injected += 1;
                skip.insert(origin);
            }
            if injected == 0 && receipts == 0 {
                let until_claim = next_claim.saturating_duration_since(Instant::now());
                self.mux.wait(until_claim.clamp(Duration::from_millis(5), Duration::from_millis(50)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use conde_remote::Badge;

    use super::*;

    fn notify(kind: NotifyKind, place: &str, member: bool, badges: Vec<Badge>) -> Notify {
        Notify {
            notification_id: "n".into(),
            attempt_id: "a".into(),
            recipient: "kdobmayer/worker".into(),
            binding_version: 3,
            endpoint: Endpoint::Mailbox { mailbox_id: "01MB".into() },
            message_id: "01MSG".into(),
            text: "conde: the v1 text, never shown on v2".into(),
            fields: Some(NotifyFields { kind, sender: "balazs-rozsenich/relay".into(), place: place.into(), badges, member }),
        }
    }

    #[test]
    fn the_text_comes_from_the_structured_fields_with_the_alias() {
        let n = notify(NotifyKind::Dm, "dm", true, Vec::new());
        assert_eq!(
            render("dakai", &n, n.fields.as_ref().unwrap()),
            "conde [dakai]: new message dakai:01MSG from balazs-rozsenich/relay. Read it with: conde read dakai:01MSG"
        );
        let n = notify(NotifyKind::Mention, "#dev", false, vec![Badge { place: "#ops".into(), unread: 2 }]);
        assert_eq!(
            render("dakai", &n, n.fields.as_ref().unwrap()),
            "conde [dakai]: you were mentioned in #dev by balazs-rozsenich/relay (message dakai:01MSG). Read it with: \
             conde read '#dev' --board dakai after joining: conde join '#dev' --board dakai | Also unread on dakai: #ops 2"
        );
        let n = notify(NotifyKind::Post, "#dev", true, Vec::new());
        let text = render("dakai", &n, n.fields.as_ref().unwrap());
        assert_eq!(
            text,
            "conde [dakai]: balazs-rozsenich/relay posted in #dev (message dakai:01MSG). Read it with: conde read '#dev' --board dakai"
        );
        assert!(!text.contains("v1 text"));
    }
}
