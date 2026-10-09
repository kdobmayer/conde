//! Remote mode for conde: the HTTPS/WebSocket server side (`/v1/rpc`, `/v1/bridge`,
//! `/healthz`) and the bridge client that holds one authenticated WebSocket to it.
//!
//! The board itself (store, dispatcher, notification state machine) is injected through
//! the [`Dispatcher`], [`Authenticator`] and [`NotificationSource`] traits, so this crate
//! carries only the transport. Frames follow DESIGN.md §4: one JSON object per WebSocket
//! text message, every frame carries `"v":1` and a `"type"`.

mod client;
mod protocol;
mod server;
pub mod web;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use client::{
    BridgeClient, BridgeConfig, BridgeState, CallError, Inbound, MAX_REASON_BYTES, StopReason, rpc,
};
pub use server::{MAX_FRAME_BYTES, ServerConfig, ServerHandle, serve, start};

/// The authenticated caller of a remote connection, derived from its bearer token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub user_id: String,
    pub host_id: String,
}

/// A signed-in browser user: reads as the owner of their agents, never through a host
/// credential and never as an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPrincipal {
    pub user_id: i64,
    pub user: String,
}

/// Executes one request object (DESIGN.md §4) for an authenticated principal and returns
/// one response object. Host and user are stamped from `principal`, never from the body.
pub trait Dispatcher: Send + Sync {
    fn dispatch(&self, principal: &Principal, request: serde_json::Value) -> serde_json::Value;
    /// A request that arrived on `bridge_instance`'s bridge connection; the board
    /// refuses it once that instance is fenced. Default: [`Dispatcher::dispatch`].
    fn dispatch_bridge(&self, principal: &Principal, _bridge_instance: &str, request: serde_json::Value) -> serde_json::Value {
        self.dispatch(principal, request)
    }
    /// A read from a signed-in browser, executed for the owner without an actor (the request's
    /// `actor` and `from` are dropped). Default: refused.
    fn dispatch_web(&self, _user: &WebPrincipal, request: serde_json::Value) -> serde_json::Value {
        let id = request.get("request_id").cloned().unwrap_or(serde_json::Value::Null);
        serde_json::json!({"v": 1, "type": "response", "request_id": id, "ok": false,
            "error": {"code": "forbidden", "message": "this server takes no browser requests"}})
    }
    /// Whether the store is reachable; `GET /healthz` answers 503 otherwise.
    fn healthy(&self) -> bool {
        true
    }
}

/// Maps a bearer token to its principal; an unknown or revoked token yields `None`.
/// The store keeps only [`token_digest`] of each token.
pub trait Authenticator: Send + Sync {
    fn authenticate(&self, bearer_token: &str) -> Option<Principal>;
    /// Like [`authenticate`](Self::authenticate), but tells a storage failure apart from an
    /// invalid token. Default: every `None` is [`Authentication::Invalid`].
    fn check(&self, bearer_token: &str) -> Authentication {
        match self.authenticate(bearer_token) {
            Some(principal) => Authentication::Valid(principal),
            None => Authentication::Invalid,
        }
    }
}

/// Result of [`Authenticator::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authentication {
    Valid(Principal),
    /// Unknown, revoked, or its user disabled: final.
    Invalid,
    /// The store did not answer; the same check may succeed later.
    Unavailable(String),
}

/// The pending-notification table (the outbox) as seen by the bridge connection.
pub trait NotificationSource: Send + Sync {
    /// Notifications currently due for delivery through this host's active bridge instance.
    fn pending_for_host(&self, host_id: &str, bridge_instance: &str) -> Vec<Notify>;
    /// Like [`NotificationSource::pending_for_host`], but tells a durably fenced
    /// instance apart from an empty outbox: the server then closes that connection.
    /// Default: never fenced.
    fn poll(&self, host_id: &str, bridge_instance: &str) -> Polled {
        Polled::Due(self.pending_for_host(host_id, bridge_instance))
    }
    /// Records an attempt outcome reported on `bridge_instance`'s connection for `host_id`;
    /// idempotent per (attempt_id, event_seq). An attempt claimed by another host or
    /// instance is [`Recorded::Rejected`]; a transient store failure is [`Recorded::Retry`].
    fn record_outcome(&self, host_id: &str, bridge_instance: &str, outcome: &Outcome) -> Recorded;
    /// Makes `bridge_instance` the host's single active bridge, or refuses an instance that a
    /// newer one already superseded.
    fn fence(&self, host_id: &str, bridge_instance: &str) -> FenceResult;
    /// Admits a well-formed hello: fences its instance in, or refuses its protocol for this
    /// host (a v1 hello from a host migrated to v2, a v2 hello from one that is not), in the
    /// same store transaction as the fence. Default: [`NotificationSource::fence`] for every
    /// protocol.
    fn admit(&self, host_id: &str, hello: &Hello) -> Admission {
        match self.fence(host_id, &hello.bridge_instance) {
            FenceResult::Active => Admission::Active,
            FenceResult::Fenced => Admission::Fenced,
        }
    }
    /// A v2 instance's poll: new attempts are claimed only up to `window` minus the instance's
    /// attempts without a first outcome, every open attempt is offered again, and the host's
    /// routes come from the same snapshot ([`Polled::Routed`]). Default: [`NotificationSource::poll`].
    fn poll_window(&self, host_id: &str, bridge_instance: &str, _window: u32) -> Polled {
        self.poll(host_id, bridge_instance)
    }
    /// The bridge's full set of live harness endpoints: from its hello (after a successful
    /// fence) and again after every `endpoints` diff frame. Default: ignored.
    fn endpoints(&self, _host_id: &str, _bridge_instance: &str, _endpoints: &[Endpoint]) {}
    /// Store events, from this or any other server replica: the server wakes the
    /// matching bridge's pending poll and closes a fenced instance's connection.
    /// Called once at start, inside the runtime. Default: none (poll only).
    fn events(&self) -> Option<tokio::sync::broadcast::Receiver<SourceEvent>> {
        None
    }
    /// The bridge answered a ping: its instance is alive (the fleet's online state).
    /// Called from a blocking thread. Default: ignored.
    fn heartbeat(&self, _host_id: &str, _bridge_instance: &str) {}
}

/// Result of [`NotificationSource::poll`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Polled {
    Due(Vec<Notify>),
    /// v2: the due notifications and the host's current routes, read in one snapshot.
    Routed { notifies: Vec<Notify>, routes: Routes },
    /// A newer instance fenced this one.
    Fenced,
}

/// Result of [`NotificationSource::admit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The instance is now the host's active bridge.
    Active,
    /// A newer instance superseded this one.
    Fenced,
    /// The hello's protocol is refused for this host; the connection is closed with `code`.
    Refused { code: String, message: String },
}

/// What the store announced after a commit (see [`NotificationSource::events`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceEvent {
    /// Pending work may exist for this host; None for every host.
    Pending(Option<String>),
    /// A newer bridge fenced this instance.
    Fenced(String),
    /// A key of this host was revoked or its user disabled: close the host's connection.
    Revoked(String),
    /// The board changed in ways the web UI shows (topic bits); the browsers refetch.
    Board(u8),
}

/// Result of [`NotificationSource::record_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Recorded now or earlier (a resend of the same attempt_id and event_seq); final.
    Accepted,
    /// Not this connection's attempt (or unknown); final, so the bridge stops resending it.
    Rejected(String),
    /// Not recorded because of a transient failure; the bridge keeps the outcome and
    /// resends it with the same attempt_id and event_seq.
    Retry(String),
}

/// Result of [`NotificationSource::fence`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceResult {
    /// The instance is now the host's active bridge; any other instance is fenced.
    Active,
    /// A newer instance superseded this one; it must not deliver any more.
    Fenced,
}

/// The bridge protocol a hello declares: 1 (harness endpoints) when `proto` is absent.
pub const PROTO_V1: u32 = 1;
/// Mailbox endpoints, an admission window and structured notifies (DESIGN-v2.md §4, §5).
pub const PROTO_V2: u32 = 2;
/// The admission window a v2 daemon declares unless configured otherwise.
pub const DEFAULT_WINDOW: u32 = 32;
/// The largest admission window a v2 hello may declare (`bridge.claim`'s limit ceiling).
pub const MAX_WINDOW: u32 = 1000;

/// The bridge's first frame (`{"v":1,"type":"hello",...}`). A v1 hello carries neither
/// `proto` nor `window` and lists harness endpoints; a v2 hello carries `"proto":2` and its
/// admission `window` and lists only mailbox endpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub host_id: String,
    pub bridge_instance: String,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default = "proto_v1", skip_serializing_if = "is_proto_v1")]
    pub proto: u32,
    /// Attempts the server may have in `dispatching` for this instance at once (v2 only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
}

fn proto_v1() -> u32 {
    PROTO_V1
}

fn is_proto_v1(proto: &u32) -> bool {
    *proto == PROTO_V1
}

impl Hello {
    /// The shape rules of a hello for its declared protocol; the error is the refusal text.
    pub fn check(&self) -> Result<(), String> {
        let mailboxes = self
            .endpoints
            .iter()
            .filter(|e| matches!(e, Endpoint::Mailbox { .. }))
            .count();
        match (self.proto, self.window) {
            (PROTO_V1, None) if mailboxes == 0 => Ok(()),
            (PROTO_V1, None) => Err("a v1 hello lists only harness endpoints".into()),
            (PROTO_V1, Some(_)) => Err("a v1 hello has no window".into()),
            (PROTO_V2, Some(w)) if !(1..=MAX_WINDOW).contains(&w) => {
                Err(format!("window {w} is outside 1..={MAX_WINDOW}"))
            }
            (PROTO_V2, Some(_)) if mailboxes == self.endpoints.len() => Ok(()),
            (PROTO_V2, Some(_)) => Err("a v2 hello lists only mailbox endpoints".into()),
            (PROTO_V2, None) => Err("a v2 hello declares its window".into()),
            (other, _) => Err(format!("unsupported bridge protocol {other}")),
        }
    }
}

/// One delivery attempt pushed to a bridge (`{"v":1,"type":"notify",...}`).
///
/// v1: `text` is the rendered notification and `fields` is absent. v2: `fields` is required
/// (a notify without all of them is refused) and `text` is empty; the daemon renders the text
/// from `fields` with the board's alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notify {
    pub notification_id: String,
    pub attempt_id: String,
    pub recipient: String,
    /// The recipient row's `binding_version`; for a mailbox, its route generation.
    pub binding_version: u64,
    pub endpoint: Endpoint,
    pub message_id: String,
    /// Always sent, since a v1 client requires it; may be absent from a v2 frame.
    #[serde(default)]
    pub text: String,
    /// Flattened into the frame; `None` unless every field is present and well formed.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub fields: Option<NotifyFields>,
}

impl Notify {
    /// The structured fields a v2 notify must carry, or `None` when they are missing.
    pub fn structured(&self) -> Option<&NotifyFields> {
        self.fields.as_ref()
    }
}

/// The structured part of a v2 notify, from which the daemon renders the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyFields {
    pub kind: NotifyKind,
    /// The sender's `owner/name` on the board.
    pub sender: String,
    /// `dm`, or `#<channel>` for a channel post.
    pub place: String,
    /// The recipient's other unread channels.
    pub badges: Vec<Badge>,
    /// The recipient is a member of `place` (always true for a DM); when false the text adds
    /// the join hint, as v1 does.
    pub member: bool,
}

/// Why the recipient is notified: a direct message, an @mention, or a channel post.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyKind {
    Dm,
    Mention,
    Post,
}

/// An unread channel of the recipient: `{"place":"#dev","unread":3}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Badge {
    pub place: String,
    pub unread: u64,
}

/// A delivery endpoint on the bridge's host. JSON: `{"kind":"claude","session_id":..}`,
/// `{"kind":"codex","thread_id":..}`, or (v2) `{"kind":"mailbox","mailbox_id":..}`, a
/// published mailbox whose session only the daemon knows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Endpoint {
    Claude { session_id: String },
    Codex { thread_id: String },
    Mailbox { mailbox_id: String },
}

/// This host's current mailbox routes (`{"v":1,"type":"routes","mailboxes":[..]}`), sent by
/// the server to a v2 bridge on connect and after any change to them; always the whole set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routes {
    pub mailboxes: Vec<Route>,
}

/// One mailbox routed to this host at its route generation (the row's `binding_version`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Route {
    pub mailbox_id: String,
    pub route_generation: u64,
}

/// An attempt outcome reported by a bridge (`{"v":1,"type":"outcome",...}`).
/// `status` is an [`OutcomeStatus`]; on v1 `reason` is free text, on v2 an [`OutcomeReason`]
/// (checked with [`Outcome::check_v2`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub attempt_id: String,
    pub event_seq: u64,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Outcome {
    /// The status and reason of an outcome on a v2 connection; an unknown status or a reason
    /// outside the closed set is refused.
    pub fn check_v2(&self) -> Result<(OutcomeStatus, Option<OutcomeReason>), String> {
        let status = OutcomeStatus::parse(&self.status)
            .ok_or_else(|| format!("unknown outcome '{}'", self.status))?;
        let reason = match self.reason.as_deref() {
            None => None,
            Some(r) => Some(
                OutcomeReason::parse(r).ok_or_else(|| format!("unknown outcome reason '{r}'"))?,
            ),
        };
        Ok((status, reason))
    }
}

/// The statuses an attempt outcome reports, with their v1 meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutcomeStatus {
    /// Handed over, no receipt yet (revisable).
    Accepted,
    /// Parked by the recipient's harness (revisable).
    Held,
    Delivered,
    Expired,
    Denied,
    Refused,
    Dropped,
    Failed,
    /// Given up by the bridge (session replaced or gone); re-attempted at once.
    Abandoned,
}

impl OutcomeStatus {
    pub const ALL: [OutcomeStatus; 9] = [
        OutcomeStatus::Accepted,
        OutcomeStatus::Held,
        OutcomeStatus::Delivered,
        OutcomeStatus::Expired,
        OutcomeStatus::Denied,
        OutcomeStatus::Refused,
        OutcomeStatus::Dropped,
        OutcomeStatus::Failed,
        OutcomeStatus::Abandoned,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeStatus::Accepted => "accepted",
            OutcomeStatus::Held => "held",
            OutcomeStatus::Delivered => "delivered",
            OutcomeStatus::Expired => "expired",
            OutcomeStatus::Denied => "denied",
            OutcomeStatus::Refused => "refused",
            OutcomeStatus::Dropped => "dropped",
            OutcomeStatus::Failed => "failed",
            OutcomeStatus::Abandoned => "abandoned",
        }
    }

    pub fn parse(s: &str) -> Option<OutcomeStatus> {
        OutcomeStatus::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether the status closes its attempt (everything but accepted and held).
    pub fn is_terminal(self) -> bool {
        !matches!(self, OutcomeStatus::Accepted | OutcomeStatus::Held)
    }
}

/// The closed set of outcome reasons on a v2 connection (DESIGN-v2.md §5). Detailed
/// diagnostics stay in the local log and `doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutcomeReason {
    #[serde(rename = "recipient unavailable")]
    RecipientUnavailable,
    #[serde(rename = "recipient offline")]
    RecipientOffline,
    #[serde(rename = "recipient session replaced")]
    RecipientSessionReplaced,
    #[serde(rename = "transport error")]
    TransportError,
    #[serde(rename = "duplicate")]
    Duplicate,
    #[serde(rename = "outcome too large")]
    OutcomeTooLarge,
}

impl OutcomeReason {
    pub const ALL: [OutcomeReason; 6] = [
        OutcomeReason::RecipientUnavailable,
        OutcomeReason::RecipientOffline,
        OutcomeReason::RecipientSessionReplaced,
        OutcomeReason::TransportError,
        OutcomeReason::Duplicate,
        OutcomeReason::OutcomeTooLarge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeReason::RecipientUnavailable => "recipient unavailable",
            OutcomeReason::RecipientOffline => "recipient offline",
            OutcomeReason::RecipientSessionReplaced => "recipient session replaced",
            OutcomeReason::TransportError => "transport error",
            OutcomeReason::Duplicate => "duplicate",
            OutcomeReason::OutcomeTooLarge => "outcome too large",
        }
    }

    pub fn parse(s: &str) -> Option<OutcomeReason> {
        OutcomeReason::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Lowercase hex SHA-256 of a bearer token: the form the store keeps and looks up.
pub fn token_digest(bearer_token: &str) -> String {
    Sha256::digest(bearer_token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_digest_is_hex_sha256() {
        assert_eq!(
            token_digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn endpoint_json_shape() {
        let e = Endpoint::Codex {
            thread_id: "t1".into(),
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"kind":"codex","thread_id":"t1"})
        );
    }
}
