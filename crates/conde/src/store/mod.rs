//! The board's storage seam: one trait, two backends. SQLite (rusqlite,
//! bundled, WAL) serves local mode; Postgres serves the cloud server, where
//! several stateless replicas share one database.
//!
//! The trait is synchronous on purpose. Every op is a plain function that
//! interleaves branching with many small queries inside one transaction, and
//! both transports already run the dispatcher on blocking threads
//! (`spawn_blocking`); a sync `Tx` keeps the op code identical on both
//! backends. Consequence: `Store` and `Tx` methods must not be called from
//! inside an async task, only from a blocking thread.
//!
//! Isolation is the same on both backends: write transactions are serialized
//! (SQLite: one connection behind a mutex plus `BEGIN IMMEDIATE`; Postgres: a
//! transaction-scoped advisory lock), so read-then-write ops stay correct and
//! `commit_seq` order equals commit order, which cursor paging relies on. Read
//! transactions see one consistent snapshot.
//!
//! A `Tx` rolls back when dropped without `commit`, so a failing op leaves no
//! partial mutation, idempotency record, attachment or notification behind.

pub mod local;
pub mod postgres;
pub mod sqlite;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::broadcast;

use crate::address::{Address, Kind};

#[derive(Debug, Error)]
#[error("{0}")]
pub struct StoreError(pub String);

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// One consistent snapshot; writing in it is an error on Postgres.
    Read,
    /// Serialized against every other write transaction on the board.
    Write,
}

pub trait Store: Send + Sync {
    fn begin(&self, mode: Mode) -> StoreResult<Box<dyn Tx + '_>>;
    /// Wake-ups for pending-notification pollers (see [`PendingSignal`]).
    fn pending(&self) -> &PendingSignal;
    /// "sqlite" or "postgres".
    fn backend(&self) -> &'static str;
    /// Readiness: the store answers a trivial query.
    fn ping(&self) -> bool;
    /// The concrete backend (tests reach SQLite's connection through it).
    fn as_any(&self) -> &dyn std::any::Any;
}

// --- records ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub owner_id: i64,
    pub owner: String,
    pub name: String,
    pub display: Option<String>,
    pub address: Option<Address>,
    pub binding_version: i64,
    pub pinned: Option<String>,
}

impl Agent {
    /// `owner/dotted.name`, the form every listing shows.
    pub fn canonical(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    pub fn is_human(&self) -> bool {
        self.address.as_ref().is_some_and(|a| a.kind == Kind::Human)
    }
}

/// Builds the address from the three nullable columns; any NULL means unbound.
pub(crate) fn address_of(kind: Option<String>, target: Option<String>, host: Option<String>) -> Option<Address> {
    match (kind.as_deref().and_then(Kind::parse), target, host) {
        (Some(k), Some(t), Some(h)) => Some(Address::new(k, &t, &h)),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    /// The creating agent's id.
    pub created_by: String,
    pub topic: Option<String>,
}

/// A message with its parties' canonical names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub seq: i64,
    pub id: String,
    pub sender_id: String,
    pub sender: String,
    pub recipient_id: Option<String>,
    pub recipient: Option<String>,
    pub channel_id: Option<String>,
    pub channel: Option<String>,
    pub text: String,
    pub reply_to: Option<String>,
    pub created_at: String,
}

impl Message {
    pub fn place(&self) -> String {
        self.channel.as_ref().map_or_else(|| "dm".to_string(), |c| format!("#{c}"))
    }

    pub fn to(&self) -> String {
        self.channel.as_ref().map(|c| format!("#{c}")).or_else(|| self.recipient.clone()).unwrap_or_default()
    }

    pub fn preview(&self) -> String {
        let first = self.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        first.chars().take(80).collect()
    }
}

/// Exactly one of `recipient_id` / `channel_id` is set.
#[derive(Clone, Copy, Debug)]
pub struct NewMessage<'a> {
    pub id: &'a str,
    pub sender_id: &'a str,
    pub recipient_id: Option<&'a str>,
    pub channel_id: Option<&'a str>,
    pub text: &'a str,
    pub reply_to: Option<&'a str>,
    pub created_at: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub id: String,
    pub filename: String,
    pub size: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub message_id: String,
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSummary {
    pub name: String,
    pub topic: Option<String>,
    pub members: i64,
    pub messages: i64,
}

/// The indexed column one range of the owner feed walks (`messages_by_sender`,
/// `messages_by_recipient`, `messages_by_channel`, each with `commit_seq` second).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedBy {
    Sender,
    Recipient,
    Channel,
}

/// Whose agent a feed row has on its other side: one of the owner's, another owner's, or anyone's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    Any,
    Own,
    Other,
}

/// The channel posts one feed range keeps.
#[derive(Clone, Debug)]
pub enum FeedPosts {
    /// A sender range: the sender's posts in these channels (by id).
    In(Vec<String>),
    /// A channel range: posts whose sender is on this side.
    By(Party),
}

/// One index range of `owner.feed`: the messages of one sender, DM recipient or channel,
/// narrowed to the DMs and channel posts it keeps.
#[derive(Clone, Debug)]
pub struct FeedRange {
    pub by: FeedBy,
    pub id: String,
    /// The DMs a sender or recipient range keeps, by the party at the other end.
    pub dms: Option<Party>,
    pub posts: Option<FeedPosts>,
}

/// An agent of an owner in a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Membership {
    pub agent_id: String,
    /// The agent's canonical name.
    pub agent: String,
    pub channel_id: String,
    pub channel: String,
}

/// A parameter of [`feed_seqs_sql`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedArg {
    Int(i64),
    Text(String),
}

/// The seqs of the newest messages over `ranges` with `after < seq < before`, at most `limit`, in
/// one statement for either store (`ph(n)` writes parameter n), with its parameters in order (the
/// owner id only when a range names a side, so every parameter is used). Each range walks its
/// index newest first and stops at `limit`; UNION merges them without duplicates (a DM between
/// two own agents sits in a sender and a recipient range). None when no range keeps anything.
pub(crate) fn feed_seqs_sql(
    owner_id: i64,
    ranges: &[FeedRange],
    after: i64,
    before: i64,
    limit: i64,
    ph: impl Fn(usize) -> String,
) -> Option<(String, Vec<FeedArg>)> {
    let mut args = vec![FeedArg::Int(after), FeedArg::Int(before), FeedArg::Int(limit)];
    let (after, before, limit) = (ph(1), ph(2), ph(3));
    let mut arg = |a: FeedArg| {
        if let Some(n) = args.iter().position(|x| *x == a) {
            return ph(n + 1);
        }
        args.push(a);
        ph(args.len())
    };
    let sided = |p: Option<Party>| matches!(p, Some(Party::Own | Party::Other));
    let names_a_side = ranges.iter().any(|r| sided(r.dms) || matches!(r.posts, Some(FeedPosts::By(p)) if sided(Some(p))));
    let own = if names_a_side {
        format!("(SELECT agent_id FROM agents WHERE owner_id = {})", arg(FeedArg::Int(owner_id)))
    } else {
        String::new()
    };
    let side = |column: &str, p: Party| match p {
        Party::Any => None,
        Party::Own => Some(format!("{column} IN {own}")),
        Party::Other => Some(format!("{column} NOT IN {own}")),
    };
    let mut terms = Vec::new();
    for r in ranges {
        let mut keep = Vec::new();
        // A recipient range is DMs only (a DM recipient is never set on a channel post); naming
        // `channel_id IS NULL` there would offer SQLite messages_by_channel, every DM on the board.
        match (r.dms, r.by) {
            (Some(p), FeedBy::Recipient) => keep.push(side("sender_agent_id", p).unwrap_or_else(|| "1 = 1".into())),
            (Some(p), _) => keep.push(
                std::iter::once("channel_id IS NULL".to_string()).chain(side("recipient_agent_id", p)).collect::<Vec<_>>().join(" AND "),
            ),
            (None, _) => {}
        }
        match &r.posts {
            Some(FeedPosts::In(ids)) if !ids.is_empty() => {
                let list: Vec<String> = ids.iter().map(|c| arg(FeedArg::Text(c.clone()))).collect();
                keep.push(format!("channel_id IN ({})", list.join(", ")));
            }
            Some(FeedPosts::By(p)) => keep.push(side("sender_agent_id", *p).unwrap_or_else(|| "1 = 1".into())),
            _ => {}
        }
        if keep.is_empty() {
            continue;
        }
        let column = match r.by {
            FeedBy::Sender => "sender_agent_id",
            FeedBy::Recipient => "recipient_agent_id",
            FeedBy::Channel => "channel_id",
        };
        let id = arg(FeedArg::Text(r.id.clone()));
        terms.push(format!(
            "SELECT commit_seq FROM (SELECT commit_seq FROM messages WHERE {column} = {id} AND commit_seq > {after}
                 AND commit_seq < {before} AND ({keep}) ORDER BY commit_seq DESC LIMIT {limit}) AS r{n}",
            keep = keep.join(" OR "),
            n = terms.len()
        ));
    }
    if terms.is_empty() {
        return None;
    }
    // SQLite caps one compound SELECT at 500 terms: union them in groups.
    let groups: Vec<String> = terms
        .chunks(100)
        .enumerate()
        .map(|(n, g)| format!("SELECT commit_seq FROM ({}) AS g{n}", g.join(" UNION ")))
        .collect();
    let sql = format!("SELECT commit_seq FROM ({}) AS u ORDER BY commit_seq DESC LIMIT {limit}", groups.join(" UNION "));
    Some((sql, args))
}

/// Which messages a search scans, on top of what the caller may see.
#[derive(Clone, Copy, Debug)]
pub enum SearchScope<'a> {
    All,
    /// One channel, by normalized name.
    Channel(&'a str),
    /// DMs with one partner, by agent id.
    Partner(&'a str),
}

#[derive(Clone, Copy, Debug)]
pub struct NewNotification<'a> {
    pub id: &'a str,
    pub message_id: &'a str,
    pub recipient_id: &'a str,
    /// dm | mention | post
    pub kind: &'a str,
    pub binding_version: i64,
    pub next_attempt_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueNotification {
    pub id: String,
    pub message_id: String,
    pub recipient_id: String,
    pub kind: String,
}

/// A notification as tests and diagnostics see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRow {
    pub id: String,
    pub message_id: String,
    pub recipient_id: String,
    pub recipient_name: String,
    pub kind: String,
    pub status: String,
    pub next_attempt_at: i64,
}

/// An attempt as tests and diagnostics see it, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRow {
    pub id: String,
    pub notification_id: String,
    pub bridge_instance: String,
    pub target_binding_version: i64,
    pub last_event_seq: i64,
    pub status: String,
    pub reason: Option<String>,
}

/// A DM's notification status with its latest attempt's outcome, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmDelivery {
    pub status: String,
    pub last_attempt: Option<String>,
    pub reason: Option<String>,
}

/// One DM conversation of one of an owner's agents, for the web UI's rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmThread {
    /// The owner's agent and its canonical name.
    pub agent_id: String,
    pub agent: String,
    pub partner_id: String,
    /// The partner's canonical name.
    pub partner: String,
    pub last_seq: i64,
    pub last_at: String,
    /// DMs from the partner the agent has not read.
    pub unread: i64,
}

/// A channel one of an owner's agents belongs to, for the web UI's rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelThread {
    /// The owner's agent and its canonical name.
    pub agent_id: String,
    pub agent: String,
    pub name: String,
    pub topic: Option<String>,
    /// 0 for an empty channel.
    pub last_seq: i64,
    pub last_at: Option<String>,
    /// Posts by others past the agent's read position.
    pub unread: i64,
}

/// One notification of a message with its latest attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub recipient_id: String,
    /// The recipient's canonical name.
    pub recipient: String,
    /// dm | mention | post
    pub kind: String,
    pub status: String,
    /// Unix milliseconds of the next attempt (meaningful while pending).
    pub next_attempt_at: i64,
    pub attempts: i64,
    pub last_status: Option<String>,
    pub last_reason: Option<String>,
}

/// A host's bridge as the fleet view sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBridge {
    pub host: String,
    pub owner: String,
    /// The latest heartbeat of the host's active bridge instance.
    pub heartbeat: Option<String>,
    /// The latest activity of any of the host's bridge instances.
    pub last_seen: Option<String>,
}

/// A live harness endpoint as the host's bridge last reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRow {
    pub host: String,
    pub kind: String,
    pub target: String,
    pub seen_at: String,
}

/// Delivery health of one recipient agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHealth {
    pub agent_id: String,
    /// Notifications held at the recipient for approval.
    pub held: i64,
    /// Notifications still pending or dispatching for a message older than the cutoff.
    pub stuck: i64,
    /// When the newest attempt reached the recipient (delivered; accepted for Codex); None when
    /// none did, or when the ones that did carry no time (recorded before `settled_at` existed
    /// or by an older server during a roll).
    pub last_reached: Option<String>,
    /// Whether any attempt ever reached the recipient, whatever its recorded time.
    pub reached: bool,
}

/// A host credential as the admin lists it (the digest, never the key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRow {
    pub digest: String,
    pub user: String,
    pub host: String,
    pub label: Option<String>,
    pub created_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// A user as browser sign-in and the admin API see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub user_id: i64,
    pub name: String,
    pub disabled: bool,
    /// Whether an Entra identity is bound to the user.
    pub bound: bool,
}

/// A browser session (unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSessionRow {
    pub user_id: i64,
    pub user_name: String,
    pub display: String,
    pub csrf: String,
    pub created_at: i64,
    pub last_seen: i64,
}

/// A remote MCP grant: one hosted client's consented access for a user (unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpGrantRow {
    pub grant_id: String,
    pub user_id: i64,
    pub client_id: String,
    pub client_host: String,
    pub client_name: Option<String>,
    /// The resource (MCP endpoint URL) the grant was consented for: its tokens' audience.
    pub resource: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

/// A one-time authorization code, keyed by its digest (unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCodeRow {
    pub user_id: i64,
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uri: String,
    pub challenge: String,
    pub resource: String,
    pub created_at: i64,
}

/// One refresh token of a grant's family; `consumed_at` is set when it was rotated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpRefreshRow {
    pub grant_id: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub consumed_at: Option<i64>,
}

/// An attempt a new bridge instance or a rebind outdated, with what its notification is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleAttempt {
    pub attempt_id: String,
    /// 'bridge instance gone' or 'recipient rebound'.
    pub reason: String,
    pub notification_id: String,
    pub message_id: String,
    pub recipient_id: String,
}

/// An attempt still open on a bridge instance, as it is offered again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAttempt {
    pub attempt_id: String,
    pub notification_id: String,
    pub address: String,
    pub text: String,
    pub recipient: String,
    pub message_id: String,
    pub binding_version: i64,
    /// The notification's kind: dm | mention | post.
    pub kind: String,
    pub recipient_id: String,
}

/// One message that relates an agent bound on a host to an agent of another owner (the
/// migration preview's evidence, DESIGN-v2.md §9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceRow {
    pub agent_id: String,
    /// dm | channel
    pub via: String,
    /// The other owner's agent (`owner/name`) for a DM, `#channel` for a channel post.
    pub with: String,
    pub created_at: String,
    /// The board created no notification for the message and it is dated before a notified
    /// message committed earlier: written by `conde import` (kond's dates), not sent here.
    pub imported: bool,
}

/// What an owner already has on the board, for `conde import`'s idempotency.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportState {
    pub user_id: Option<i64>,
    /// name -> (address, pin) of the owner's agents.
    pub agents: HashMap<String, (Option<String>, Option<String>)>,
    pub channels: HashSet<String>,
    pub messages: HashSet<String>,
    /// (message id, filename)
    pub attachments: HashSet<(String, String)>,
    /// (channel name, agent name) of the owner's agents.
    pub memberships: HashSet<(String, String)>,
    /// (message id, agent name) of the owner's agents.
    pub dm_read: HashSet<(String, String)>,
    /// (message id, agent name, emoji) of the owner's agents.
    pub reactions: HashSet<(String, String, String)>,
}

/// An agent row as `conde import` writes it, binding and profile included.
#[derive(Clone, Copy, Debug)]
pub struct ImportedAgent<'a> {
    pub id: &'a str,
    pub owner_id: i64,
    pub name: &'a str,
    pub display: Option<&'a str>,
    pub address: Option<&'a Address>,
    pub binding_version: i64,
    pub pinned: Option<&'a str>,
}

/// One transaction. Every query the dispatcher, the ops and the importer run
/// is a method here; dialect differences (upserts, GREATEST/MAX, booleans,
/// BLOB/bytea, NULL ordering, insertion order) stay inside the backends.
pub trait Tx {
    fn commit(self: Box<Self>) -> StoreResult<()>;

    // principals and credentials
    /// The user's id, creating the user on first sight.
    fn ensure_user(&self, name: &str) -> StoreResult<i64>;
    /// The id of the user owning `host`, recording `user_id` as owner on first sight.
    fn ensure_host(&self, host: &str, user_id: i64) -> StoreResult<i64>;
    fn user_id(&self, name: &str) -> StoreResult<Option<i64>>;
    /// (user id, user name) owning `host`.
    fn host_owner(&self, host: &str) -> StoreResult<Option<(i64, String)>>;
    fn insert_credential(&self, digest: &str, user_id: i64, host: &str, label: &str, created_at: &str)
    -> StoreResult<()>;
    /// Every credential, oldest first (undated first), then by host.
    fn credentials(&self) -> StoreResult<Vec<CredentialRow>>;
    /// Revokes the live credential(s) whose digest starts with `key_id`; returns how many.
    /// The hosts are announced as revoked on commit.
    fn revoke_credential(&self, key_id: &str, at: &str) -> StoreResult<u64>;
    /// Revokes every live credential of `host`, announced on commit; returns how many.
    fn revoke_host_credentials(&self, host: &str, at: &str) -> StoreResult<u64>;
    /// The host's latest bridge activity.
    fn host_last_seen(&self, host: &str) -> StoreResult<Option<String>>;
    /// Whether the host migrated to v2 (`hosts.mailbox_only`); false for an unknown host.
    fn host_mailbox_only(&self, host: &str) -> StoreResult<bool>;
    fn set_host_mailbox_only(&self, host: &str) -> StoreResult<()>;
    /// The cross-owner message relationships of `owner_id`'s agents bound on `host`: every DM
    /// with an agent of another owner, and every channel post by the agent in a channel where
    /// an agent of another owner posted or is a member. By agent, then message order.
    fn host_evidence(&self, owner_id: i64, host: &str) -> StoreResult<Vec<EvidenceRow>>;

    // users, Entra identities and browser sign-in (times in unix seconds)
    fn user_by_identity(&self, tid: &str, oid: &str) -> StoreResult<Option<UserRow>>;
    fn user_by_name(&self, name: &str) -> StoreResult<Option<UserRow>>;
    fn user_by_id(&self, user_id: i64) -> StoreResult<Option<UserRow>>;
    /// Binds (tid, oid) to the user, with the display-only profile.
    fn bind_identity(&self, user_id: i64, tid: &str, oid: &str, display: Option<&str>, upn: Option<&str>)
    -> StoreResult<()>;
    fn set_identity_profile(&self, user_id: i64, display: Option<&str>, upn: Option<&str>) -> StoreResult<()>;
    /// Sets the user's disabled flag; returns the user's hosts. Disabling also deletes the
    /// user's browser sessions and announces the hosts as revoked on commit.
    fn set_user_disabled(&self, user_id: i64, disabled: bool) -> StoreResult<Vec<String>>;
    fn insert_web_session(&self, id_digest: &str, s: &WebSessionRow) -> StoreResult<()>;
    fn web_session(&self, id_digest: &str) -> StoreResult<Option<WebSessionRow>>;
    fn touch_web_session(&self, id_digest: &str, at: i64) -> StoreResult<()>;
    fn delete_web_session(&self, id_digest: &str) -> StoreResult<()>;
    /// Stores a one-time record (login transaction, CLI code, key display) under its digest.
    fn put_web_token(&self, kind: &str, digest: &str, payload: &str, created_at: i64) -> StoreResult<()>;
    /// Returns and deletes a one-time record as (payload, created_at).
    fn take_web_token(&self, kind: &str, digest: &str) -> StoreResult<Option<(String, i64)>>;
    /// Deletes expired sessions and one-time records (`ttls` per kind, seconds).
    fn purge_web(&self, now: i64, session_idle: i64, session_max: i64, ttls: &[(&str, i64)]) -> StoreResult<()>;
    /// (user id, host) of a live credential with this digest whose user is not disabled.
    fn authenticate(&self, digest: &str) -> StoreResult<Option<(i64, String)>>;

    // idempotency
    /// (request_hash, result_json) stored for this key.
    fn idempotency_get(&self, actor_id: &str, op: &str, key: &str) -> StoreResult<Option<(String, String)>>;
    fn idempotency_put(
        &self,
        actor_id: &str,
        op: &str,
        key: &str,
        request_hash: &str,
        result_json: &str,
        created_at: &str,
    ) -> StoreResult<()>;

    // agents
    fn agent_by_id(&self, id: &str) -> StoreResult<Option<Agent>>;
    fn agent_by_name(&self, owner_id: i64, name: &str) -> StoreResult<Option<Agent>>;
    /// The owner's agents bound to `address`, by name.
    fn agents_at_address(&self, owner_id: i64, address: &Address) -> StoreResult<Vec<Agent>>;
    /// Every bound agent, by owner then name.
    fn bound_agents(&self) -> StoreResult<Vec<Agent>>;
    /// Every agent bound on `host`, by owner then name.
    fn agents_on_host(&self, host: &str) -> StoreResult<Vec<Agent>>;
    fn insert_agent(&self, id: &str, owner_id: i64, name: &str) -> StoreResult<()>;
    /// Rebinds (or unbinds) and bumps binding_version; the old and the new host are woken
    /// on commit (a moved mailbox changes both hosts' routes), and both hosts' route
    /// revision is bumped in the same transaction.
    fn set_binding(&self, agent_id: &str, address: Option<&Address>) -> StoreResult<()>;
    /// The host's route revision (`hosts.route_revision`): bumped by every binding change
    /// that touches the host, so a routes snapshot carries the causal point it was read at.
    fn route_revision(&self, host: &str) -> StoreResult<u64>;
    fn set_pin(&self, agent_id: &str, pinned: Option<&str>) -> StoreResult<()>;
    fn set_display(&self, agent_id: &str, display: Option<&str>) -> StoreResult<()>;
    /// Gives the agent a new name in place: id, binding and every reference stay.
    fn rename_agent(&self, agent_id: &str, name: &str) -> StoreResult<()>;

    // channels
    fn channel_by_name(&self, name: &str) -> StoreResult<Option<Channel>>;
    /// The channel's id alone: readable on every schema version (import preflight).
    fn channel_id_by_name(&self, name: &str) -> StoreResult<Option<String>>;
    fn insert_channel(&self, channel: &Channel) -> StoreResult<()>;
    fn set_channel_topic(&self, channel_id: &str, topic: Option<&str>) -> StoreResult<()>;
    /// Member agent ids, sorted.
    fn channel_member_ids(&self, channel_id: &str) -> StoreResult<Vec<String>>;
    /// The member's read position, or None when not a member.
    fn read_seq(&self, channel_id: &str, agent_id: &str) -> StoreResult<Option<i64>>;
    /// Moves the read position forward only.
    fn advance_cursor(&self, channel_id: &str, agent_id: &str, seq: i64) -> StoreResult<()>;
    /// Highest commit_seq posted in the channel, 0 when empty.
    fn channel_last_seq(&self, channel_id: &str) -> StoreResult<i64>;
    fn insert_membership(&self, channel_id: &str, agent_id: &str, read_seq: i64) -> StoreResult<()>;
    fn delete_membership(&self, channel_id: &str, agent_id: &str) -> StoreResult<()>;
    /// Every channel with member and message counts, by name.
    fn channel_summaries(&self) -> StoreResult<Vec<ChannelSummary>>;
    /// (canonical member name, messages behind), by name.
    fn channel_member_status(&self, channel_id: &str) -> StoreResult<Vec<(String, i64)>>;
    /// Ids of every message posted in the channel.
    fn channel_message_ids(&self, channel_id: &str) -> StoreResult<Vec<String>>;
    /// Highest commit_seq in the channel among messages with id <= `message_id`, 0 when none.
    fn channel_seq_at(&self, channel_id: &str, message_id: &str) -> StoreResult<i64>;

    // messages
    /// Returns the commit_seq.
    fn insert_message(&self, m: &NewMessage) -> StoreResult<i64>;
    /// Inserts unless the id exists; true when inserted.
    fn insert_message_if_absent(&self, m: &NewMessage) -> StoreResult<bool>;
    fn insert_attachment(&self, id: &str, message_id: &str, filename: &str, bytes: &[u8]) -> StoreResult<()>;
    fn message_by_id(&self, id: &str) -> StoreResult<Option<Message>>;
    /// Unread DMs to `agent_id` plus unread channel posts by others, after `after`.
    fn inbox(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>>;
    /// Unread messages of one channel the agent is a member of.
    fn unread_in_channel(&self, channel_id: &str, agent_id: &str, limit: i64) -> StoreResult<Vec<Message>>;
    /// Unread DMs plus unread channel messages, own posts included.
    fn unread_all(&self, agent_id: &str, limit: i64) -> StoreResult<Vec<Message>>;
    fn sent_by(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>>;
    /// The lowest commit seq of a message dated on or after `day` (`YYYY-MM-DD`, compared with
    /// the first ten characters of `created_at`, which every stored ISO 8601 date shares): where a
    /// time search can start, since no later-dated message has a lower seq. Read through the
    /// `messages_by_day` index (day, seq): the lowest seq of each day from `day` on.
    fn first_seq_from_day(&self, day: &str) -> StoreResult<Option<i64>>;
    /// Messages visible to `agent_id` (own DMs, joined channels) in `scope`,
    /// after `after`, dated on or after `from_day` when given (as [`Tx::first_seq_from_day`]
    /// compares), in commit order; the caller filters and pages.
    fn search_scan(
        &self,
        agent_id: &str,
        after: i64,
        scope: SearchScope,
        from_day: Option<&str>,
        limit: i64,
    ) -> StoreResult<Vec<Message>>;
    /// The highest commit seq on the board (0 when empty): the end of the primary key, no row
    /// read.
    fn last_seq(&self) -> StoreResult<i64>;
    /// Messages with `after < seq <= upto` in commit order (`newest`: the newest first), visible
    /// to `viewer` (own DMs, joined channels; None: every message), sent or received by `agent`
    /// and in `channel` (a normalized name) when given. Walks the primary key from `after`.
    #[allow(clippy::too_many_arguments)]
    fn feed_scan(
        &self,
        viewer: Option<&str>,
        after: i64,
        upto: i64,
        agent: Option<&str>,
        channel: Option<&str>,
        newest: bool,
        limit: i64,
    ) -> StoreResult<Vec<Message>>;
    fn attachment_list(&self, message_id: &str) -> StoreResult<Vec<AttachmentMeta>>;
    fn attachment(&self, id: &str) -> StoreResult<Option<Attachment>>;
    /// (recorded size, stored byte length) of a message's attachment by filename.
    fn attachment_sizes(&self, message_id: &str, filename: &str) -> StoreResult<Option<(i64, i64)>>;
    /// Present reactions as (emoji, canonical reactor), by emoji then reactor.
    fn reactions(&self, message_id: &str) -> StoreResult<Vec<(String, String)>>;
    fn set_reaction(&self, message_id: &str, actor_id: &str, emoji: &str, present: bool) -> StoreResult<()>;
    /// Inserts unless the (message, actor, emoji) row exists; true when inserted.
    fn insert_reaction_if_absent(&self, message_id: &str, actor_id: &str, emoji: &str, present: bool)
    -> StoreResult<bool>;
    /// Records the first read only; true when this call recorded it.
    fn mark_dm_read(&self, message_id: &str, agent_id: &str, at: &str) -> StoreResult<bool>;
    fn dm_read_at(&self, message_id: &str, agent_id: &str) -> StoreResult<Option<String>>;
    /// (members other than `except_agent` read past `seq`, members other than `except_agent`).
    fn channel_read_count(&self, channel_id: &str, except_agent: &str, seq: i64) -> StoreResult<(i64, i64)>;
    fn message_count(&self) -> StoreResult<i64>;

    // notifications and attempts
    /// Inserts a pending notification; an existing id is left untouched.
    fn insert_notification(&self, n: &NewNotification) -> StoreResult<()>;
    /// Pending notifications due at `now` whose recipient is bound on `host`,
    /// by due time then insertion order.
    fn due_notifications(&self, now: i64, host: &str, limit: i64) -> StoreResult<Vec<DueNotification>>;
    fn set_notification_status(&self, id: &str, status: &str) -> StoreResult<()>;
    /// Settles the recipient's notification of the message as read while it is still open
    /// (pending, dispatching, notified, held); true when one was.
    fn settle_read(&self, message_id: &str, recipient_id: &str) -> StoreResult<bool>;
    /// The same for every message of the channel up to `cursor` (the recipient's read cursor
    /// there); the number settled.
    fn settle_read_in_channel(&self, channel_id: &str, recipient_id: &str, cursor: i64) -> StoreResult<u64>;
    /// Sets the status and, when given, the next attempt time.
    fn settle_notification(&self, id: &str, status: &str, next_attempt_at: Option<i64>) -> StoreResult<()>;
    /// The push state of the DM `message_id` to `recipient`.
    fn dm_delivery(&self, message_id: &str, recipient: &str) -> StoreResult<Option<DmDelivery>>;
    /// Unread channel posts by others as (channel name, count), by name.
    fn unread_badges(&self, agent_id: &str) -> StoreResult<Vec<(String, i64)>>;
    /// A new attempt in status 'dispatching', claimed by `bridge_instance` on `host` with its text.
    fn insert_attempt(
        &self,
        id: &str,
        notification_id: &str,
        bridge_instance: &str,
        target_binding_version: i64,
        host: &str,
        text: &str,
    ) -> StoreResult<()>;
    /// The instance-scoped queries below see attempts of open notifications only (pending,
    /// dispatching, notified, held): an attempt left under a read or delivered one is never
    /// offered, re-held or counted again.
    /// This instance's held attempts as (attempt id, recipient address) for bound recipients.
    fn held_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<(String, String)>>;
    /// Attempts this instance claimed that no outcome settled (dispatching or held), oldest first.
    fn open_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<OpenAttempt>>;
    /// Held or dispatching attempts, and accepted ones whose notification
    /// still stands at 'notified', for recipients on `host` (or unbound), that
    /// another instance owns or that target an older binding. Only attempts of open
    /// notifications (pending, dispatching, notified, held): a read or delivered one keeps its attempts.
    fn stale_attempts(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<StaleAttempt>>;
    /// This instance's holds on Claude or mailbox recipients of `host` whose session or
    /// mailbox is not a live endpoint.
    fn gone_holds(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<String>>;
    /// The instance's attempts still without a first outcome (status 'dispatching').
    fn count_dispatching(&self, bridge_instance: &str) -> StoreResult<i64>;
    /// (notification id, status) of an attempt.
    fn attempt(&self, id: &str) -> StoreResult<Option<(String, String)>>;
    /// (notification id, message id, recipient agent id) of an attempt.
    fn attempt_target(&self, id: &str) -> StoreResult<Option<(String, String, String)>>;
    /// (target binding version, the recipient's current binding version) of an attempt.
    fn attempt_route(&self, id: &str) -> StoreResult<Option<(i64, i64)>>;
    /// (bridge instance, host, last event seq) of an attempt.
    fn attempt_owner(&self, id: &str) -> StoreResult<Option<(String, Option<String>, i64)>>;
    /// Sets status and reason, and last_event_seq when given.
    /// Sets status and reason, and last_event_seq when given; `at` is when the outcome was recorded.
    fn update_attempt(&self, id: &str, status: &str, reason: Option<&str>, event_seq: Option<i64>, at: &str)
    -> StoreResult<()>;
    /// The notification's most recently created attempt.
    fn latest_attempt(&self, notification_id: &str) -> StoreResult<Option<String>>;
    fn notifications(&self) -> StoreResult<Vec<NotificationRow>>;
    fn attempts(&self) -> StoreResult<Vec<AttemptRow>>;

    // bridge instances and endpoints
    /// (host, state) of a bridge instance.
    fn bridge_instance(&self, instance: &str) -> StoreResult<Option<(String, String)>>;
    /// The host's active bridge instances, sorted.
    fn active_bridges(&self, host: &str) -> StoreResult<Vec<String>>;
    /// Makes `instance` the host's one active bridge; returns the instances it fenced.
    fn fence(&self, host: &str, instance: &str, at: &str) -> StoreResult<Vec<String>>;
    /// Fences every active bridge instance of `host` (announced on commit); returns them.
    fn fence_host(&self, host: &str, at: &str) -> StoreResult<Vec<String>>;
    /// Replaces the host's live endpoints with `endpoints` as (kind, target).
    fn replace_endpoints(&self, host: &str, instance: &str, endpoints: &[(&str, &str)], at: &str)
    -> StoreResult<()>;
    fn count_endpoints(&self) -> StoreResult<i64>;

    // web UI: owner-scoped reads (what the owner's own agents can read)
    /// Every agent of the owner, bound or not, by name.
    fn owner_agents(&self, owner_id: i64) -> StoreResult<Vec<Agent>>;
    /// The DM conversations of the owner's agents, one row per (agent, counterpart), newest first.
    fn owner_dm_threads(&self, owner_id: i64) -> StoreResult<Vec<DmThread>>;
    /// The channels the owner's agents belong to, one row per (agent, channel), by agent then name.
    fn owner_channel_threads(&self, owner_id: i64) -> StoreResult<Vec<ChannelThread>>;
    /// DMs between `agent_id` and `partner_id` (either way) before `before`, newest first; none
    /// unless `agent_id` belongs to `owner_id`.
    fn owner_dm_history(&self, owner_id: i64, agent_id: &str, partner_id: &str, before: i64, limit: i64)
    -> StoreResult<Vec<Message>>;
    /// A channel's messages before `before`, newest first; none unless one of the owner's
    /// agents is a member.
    fn owner_channel_history(&self, owner_id: i64, channel_id: &str, before: i64, limit: i64)
    -> StoreResult<Vec<Message>>;
    /// The message, when one of the owner's agents sent it, received it as a DM, or is a member
    /// of its channel.
    fn owner_message(&self, owner_id: i64, message_id: &str) -> StoreResult<Option<Message>>;
    /// The message's notifications with their latest attempts, by recipient name.
    fn deliveries(&self, message_id: &str) -> StoreResult<Vec<Delivery>>;
    /// Every host with its owner and bridge activity, by owner then host.
    fn host_bridges(&self) -> StoreResult<Vec<HostBridge>>;
    /// Every endpoint the bridges reported live.
    fn endpoint_rows(&self) -> StoreResult<Vec<EndpointRow>>;
    /// Delivery health of the owner's agents that have notifications; `stuck_before` is an ISO
    /// time: an undelivered notification for an older message counts as stuck.
    fn agent_health(&self, owner_id: i64, stuck_before: &str) -> StoreResult<Vec<AgentHealth>>;
    /// Records a heartbeat of an active bridge instance; false when it is not active.
    fn touch_bridge(&self, instance: &str, at: &str) -> StoreResult<bool>;
    /// Announces a board change ([`topics`]) to the browsers on commit.
    fn announce(&self, topics: u8);
    /// Every channel membership of the owner's agents, by agent then channel name.
    fn owner_memberships(&self, owner_id: i64) -> StoreResult<Vec<Membership>>;
    /// The newest messages over `ranges` (see [`feed_seqs_sql`]) with `after < seq < before`, at
    /// most `limit`, newest first. The ranges decide what is read; the caller builds them from
    /// what the owner's agents can read.
    fn owner_feed(&self, owner_id: i64, ranges: &[FeedRange], after: i64, before: i64, limit: i64)
    -> StoreResult<Vec<Message>>;
    /// Like [`Tx::search_scan`] over what the owner's agents can read (the
    /// [`Tx::owner_message`] rule), after `after`, oldest first.
    fn owner_search_scan(
        &self,
        owner_id: i64,
        after: i64,
        scope: SearchScope,
        from_day: Option<&str>,
        limit: i64,
    ) -> StoreResult<Vec<Message>>;

    // remote MCP (docs/REMOTE-MCP.md §3)
    fn insert_mcp_code(&self, code_digest: &str, c: &McpCodeRow) -> StoreResult<()>;
    /// Returns and deletes the code.
    fn take_mcp_code(&self, code_digest: &str) -> StoreResult<Option<McpCodeRow>>;
    fn insert_mcp_grant(&self, g: &McpGrantRow) -> StoreResult<()>;
    fn insert_mcp_token(&self, token_digest: &str, grant_id: &str, audience: &str, expires_at: i64) -> StoreResult<()>;
    fn insert_mcp_refresh(&self, digest: &str, grant_id: &str, issued_at: i64, expires_at: i64) -> StoreResult<()>;
    fn mcp_grant(&self, grant_id: &str) -> StoreResult<Option<McpGrantRow>>;
    /// The access token's (grant_id, audience, expires_at).
    fn mcp_token(&self, token_digest: &str) -> StoreResult<Option<(String, String, i64)>>;
    fn mcp_refresh(&self, digest: &str) -> StoreResult<Option<McpRefreshRow>>;
    /// Marks an unconsumed refresh token consumed; false when it already was (or is unknown).
    fn consume_mcp_refresh(&self, digest: &str, at: i64) -> StoreResult<bool>;
    fn touch_mcp_grant(&self, grant_id: &str, at: i64) -> StoreResult<()>;
    /// Ends a live grant: sets revoked_at and deletes its access and refresh tokens. False when
    /// the grant was not live.
    fn revoke_mcp_grant(&self, grant_id: &str, at: i64) -> StoreResult<bool>;
    /// The authorization code with this digest, while it waits to be redeemed.
    fn mcp_code(&self, code_digest: &str) -> StoreResult<Option<McpCodeRow>>;
    /// Records the authorization code (its digest) the grant was minted from.
    fn set_mcp_grant_code(&self, grant_id: &str, code_digest: &str) -> StoreResult<()>;
    /// The stored grant minted from the authorization code.
    fn mcp_grant_of_code(&self, code_digest: &str) -> StoreResult<Option<String>>;
    /// The user's grants, newest first.
    fn mcp_grants_of(&self, user_id: i64) -> StoreResult<Vec<McpGrantRow>>;
    /// Deletes codes older than `code_ttl`, expired access tokens, expired unconsumed refresh
    /// tokens, and grants expired or revoked (with their tokens). A consumed refresh token is
    /// kept until its grant ends, so a replay within the grant's life still revokes it.
    fn purge_mcp(&self, now: i64, code_ttl: i64) -> StoreResult<()>;

    // import
    fn import_state(&self, owner: &str) -> StoreResult<ImportState>;
    fn insert_imported_agent(&self, a: &ImportedAgent) -> StoreResult<()>;

    // diagnostics
    /// "ok" when the store is consistent, else the backend's finding.
    fn integrity_check(&self) -> StoreResult<String>;
    fn count_bound_agents(&self) -> StoreResult<i64>;
    /// The owner's unbound agents with pending notifications as (canonical name, count), by name.
    fn unbound_waiting(&self, owner_id: i64) -> StoreResult<Vec<(String, i64)>>;
    /// Notifications in `statuses` whose recipient agent belongs to the owner.
    fn count_notifications(&self, owner_id: i64, statuses: &[&str]) -> StoreResult<i64>;
}

/// The migration preview's evidence (DESIGN-v2.md §9), one row per relating message. A message is
/// imported when the board created no notification for it and it is dated before a notified
/// message committed earlier: the importer creates none and writes kond's dates, while traffic
/// sent here is dated at its commit.
/// Both dialects: SQLite binds `$1`, `$2` by position.
pub(crate) const HOST_EVIDENCE: &str = "WITH notified AS (SELECT DISTINCT message_id FROM notifications),
    dated AS (
        SELECT m.message_id, m.created_at, n.message_id IS NOT NULL AS native,
               MAX(CASE WHEN n.message_id IS NOT NULL THEN m.created_at END)
                   OVER (ORDER BY m.commit_seq ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS prior
        FROM messages m LEFT JOIN notified n ON n.message_id = m.message_id),
    mine AS (SELECT agent_id, owner_id FROM agents WHERE owner_id = $1 AND host_id = $2 AND address_kind IS NOT NULL),
    ev AS (
        SELECT r.agent_id, 'dm' AS via, ou.name || '/' || o.name_lower AS partner, m.message_id, m.commit_seq
        FROM mine r
        JOIN messages m ON m.channel_id IS NULL AND (m.sender_agent_id = r.agent_id OR m.recipient_agent_id = r.agent_id)
        JOIN agents o ON o.agent_id = CASE WHEN m.sender_agent_id = r.agent_id THEN m.recipient_agent_id
                                           ELSE m.sender_agent_id END
        JOIN users ou ON ou.user_id = o.owner_id
        WHERE o.owner_id <> r.owner_id
        UNION ALL
        SELECT r.agent_id, 'channel', '#' || c.name_lower, m.message_id, m.commit_seq
        FROM mine r
        JOIN messages m ON m.sender_agent_id = r.agent_id AND m.channel_id IS NOT NULL
        JOIN channels c ON c.channel_id = m.channel_id
        WHERE EXISTS (SELECT 1 FROM memberships x JOIN agents xa ON xa.agent_id = x.agent_id
                      WHERE x.channel_id = m.channel_id AND xa.owner_id <> r.owner_id)
           OR EXISTS (SELECT 1 FROM messages p JOIN agents pa ON pa.agent_id = p.sender_agent_id
                      WHERE p.channel_id = m.channel_id AND pa.owner_id <> r.owner_id))
    SELECT ev.agent_id, ev.via, ev.partner, d.created_at,
           CASE WHEN NOT d.native AND d.prior IS NOT NULL AND d.created_at < d.prior THEN 'imported' ELSE 'later' END
    FROM ev JOIN dated d ON d.message_id = ev.message_id
    ORDER BY ev.agent_id, ev.commit_seq";

// --- events -------------------------------------------------------------------

/// What a committed write tells the servers, on every replica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Pending work for recipients bound on this host.
    Host(String),
    /// Anything may have changed (e.g. events were missed while the Postgres
    /// listener reconnected).
    All,
    /// This bridge instance was fenced by a newer one: close its connection.
    Fenced(String),
    /// A key of this host was revoked or its user disabled: close the host's connection.
    Revoked(String),
    /// The board changed in ways the web UI shows ([`topics`] bits); carries no content.
    Board(u8),
}

/// What a [`Wake::Board`] says changed.
pub mod topics {
    /// Messages, read positions, reactions, memberships.
    pub const MESSAGES: u8 = 1;
    /// Notifications and attempts.
    pub const DELIVERY: u8 = 2;
    /// Bindings, pins, profiles, bridges, endpoints.
    pub const FLEET: u8 = 4;
    pub const ALL: u8 = MESSAGES | DELIVERY | FLEET;
}

/// Raised after a write transaction commits that made a notification pending
/// (new, retried or abandoned), rebound an agent onto a host, or fenced a
/// bridge instance. SQLite raises it in-process; Postgres sends NOTIFY in the
/// transaction and every replica's listener raises it on receipt, so a bridge
/// connected to any replica hears it. Pollers still poll on a fallback
/// interval: a wake is a hint, not a queue.
pub struct PendingSignal {
    tx: broadcast::Sender<Wake>,
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Default for PendingSignal {
    fn default() -> PendingSignal {
        PendingSignal { tx: broadcast::channel(256).0, generation: Mutex::new(0), changed: Condvar::new() }
    }
}

impl PendingSignal {
    pub fn raise(&self, wake: Wake) {
        let pending = !matches!(wake, Wake::Fenced(_) | Wake::Revoked(_) | Wake::Board(_));
        let _ = self.tx.send(wake);
        if pending {
            *self.generation.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            self.changed.notify_all();
        }
    }

    pub fn subscribe(&self) -> PendingWatch {
        PendingWatch { rx: self.tx.subscribe() }
    }

    /// Every event, fences included (the server's connection registry).
    pub fn events(&self) -> broadcast::Receiver<Wake> {
        self.tx.subscribe()
    }

    /// Blocks until any pending-work wake or `fallback` elapses; true when
    /// woken. For blocking-thread pollers (the local daemon's bridge loop).
    pub fn wait_blocking(&self, fallback: Duration) -> bool {
        let deadline = Instant::now() + fallback;
        let mut generation = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        let start = *generation;
        while *generation == start {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            generation = self.changed.wait_timeout(generation, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }
}

/// One poller's subscription to [`PendingSignal`].
pub struct PendingWatch {
    rx: broadcast::Receiver<Wake>,
}

impl PendingWatch {
    /// Returns true when work for `host` may be pending, false after
    /// `fallback` without a matching wake.
    pub async fn wait(&mut self, host: &str, fallback: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + fallback;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Err(_) => return false,
                Ok(Ok(Wake::Host(h))) if h != host => continue,
                Ok(Ok(Wake::Fenced(_) | Wake::Revoked(_) | Wake::Board(_))) => continue,
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => return true,
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    tokio::time::sleep_until(deadline).await;
                    return false;
                }
            }
        }
    }
}

/// What to announce after a commit, collected while the transaction runs.
#[derive(Default)]
pub(crate) struct Wakes {
    /// Notifications made pending; their recipients' hosts are looked up at commit.
    pub notifications: BTreeSet<String>,
    /// Hosts an agent was bound onto (its pending notifications became claimable there).
    pub hosts: BTreeSet<String>,
    /// Bridge instances fenced by this transaction.
    pub fenced: BTreeSet<String>,
    /// Hosts whose keys this transaction revoked (or whose user it disabled).
    pub revoked: BTreeSet<String>,
    /// Board changes announced to the browsers ([`topics`] bits).
    pub board: u8,
}

impl Wakes {
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
            && self.hosts.is_empty()
            && self.fenced.is_empty()
            && self.revoked.is_empty()
            && self.board == 0
    }
}
