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
    BridgeClient, BridgeConfig, BridgeState, CallError, MAX_REASON_BYTES, StopReason,
};
pub use server::{MAX_FRAME_BYTES, ServerConfig, ServerHandle, serve, start};

/// The authenticated caller of a remote connection, derived from its bearer token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub user_id: String,
    pub host_id: String,
}

/// A signed-in browser user: acts as their human, never through a host credential.
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
    /// A request from a signed-in browser, executed as the user's human; the request's
    /// `actor` and `from` are the server's to set, never the browser's. Default: refused.
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
    /// A newer instance fenced this one.
    Fenced,
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

/// One delivery attempt pushed to a bridge (`{"v":1,"type":"notify",...}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notify {
    pub notification_id: String,
    pub attempt_id: String,
    pub recipient: String,
    pub binding_version: u64,
    pub endpoint: Endpoint,
    pub message_id: String,
    pub text: String,
}

/// A harness endpoint on the bridge's host. JSON: `{"kind":"claude","session_id":..}` or
/// `{"kind":"codex","thread_id":..}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Endpoint {
    Claude { session_id: String },
    Codex { thread_id: String },
}

/// An attempt outcome reported by a bridge (`{"v":1,"type":"outcome",...}`).
/// `status` is one of held|delivered|expired|denied|refused|dropped|accepted|failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub attempt_id: String,
    pub event_seq: u64,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
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
