//! Rows of the local board's own tables (`publications`, `board_ops`; DESIGN-v2.md §4). They
//! exist in SQLite only and never reach a server; the values match the schema's CHECKs.

use serde_json::Value;

use crate::protocol::Request;
use crate::protocol::v2::op;

/// A local agent's publication on one shared board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    /// The shared board's local alias.
    pub board: String,
    /// The local agent.
    pub agent_id: String,
    /// The name part of `owner/name` on that board.
    pub shared_name: String,
    /// The board's agent id; `None` exactly while publishing.
    pub mailbox_id: Option<String>,
    /// The mailbox's `binding_version` on the board; set with `mailbox_id`.
    pub route_generation: Option<u64>,
    pub state: PublicationState,
    pub updated_at: String,
}

/// Only `Published` admits inbound notifies and outbound use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PublicationState {
    Publishing,
    Published,
    Unpublishing,
}

impl PublicationState {
    pub const ALL: [PublicationState; 3] =
        [PublicationState::Publishing, PublicationState::Published, PublicationState::Unpublishing];

    pub fn as_str(self) -> &'static str {
        match self {
            PublicationState::Publishing => "publishing",
            PublicationState::Published => "published",
            PublicationState::Unpublishing => "unpublishing",
        }
    }

    pub fn parse(s: &str) -> Option<PublicationState> {
        PublicationState::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// One remote mutation, recorded before its first call so a crash resumes it.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardOp {
    /// ULID; also the idempotency key sent to the board.
    pub op_id: String,
    pub board: String,
    pub kind: BoardOpKind,
    /// The local agent of a publish or unpublish; `None` for a migrate.
    pub agent_id: Option<String>,
    /// The exact body sent, every time the op is (re)sent.
    pub request: Value,
    pub state: BoardOpState,
    /// The board's answer, kept for audit.
    pub result: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl BoardOp {
    /// The request frame for this op: its body, keyed by the op id.
    pub fn request(&self, request_id: &str) -> Request {
        let mut r = Request::new(request_id, self.kind.op_name(), self.request.clone());
        r.idempotency_key = Some(self.op_id.clone());
        r
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoardOpKind {
    Publish,
    Unpublish,
    Migrate,
}

impl BoardOpKind {
    pub const ALL: [BoardOpKind; 3] = [BoardOpKind::Publish, BoardOpKind::Unpublish, BoardOpKind::Migrate];

    pub fn as_str(self) -> &'static str {
        match self {
            BoardOpKind::Publish => "publish",
            BoardOpKind::Unpublish => "unpublish",
            BoardOpKind::Migrate => "migrate",
        }
    }

    pub fn parse(s: &str) -> Option<BoardOpKind> {
        BoardOpKind::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// The board op this record sends.
    pub fn op_name(self) -> &'static str {
        match self {
            BoardOpKind::Publish => op::MAILBOX_PUBLISH,
            BoardOpKind::Unpublish => op::MAILBOX_UNPUBLISH,
            BoardOpKind::Migrate => op::HOST_MIGRATE_V2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoardOpState {
    Pending,
    Done,
    Failed,
}

impl BoardOpState {
    pub const ALL: [BoardOpState; 3] = [BoardOpState::Pending, BoardOpState::Done, BoardOpState::Failed];

    pub fn as_str(self) -> &'static str {
        match self {
            BoardOpState::Pending => "pending",
            BoardOpState::Done => "done",
            BoardOpState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<BoardOpState> {
        BoardOpState::ALL.into_iter().find(|v| v.as_str() == s)
    }
}
