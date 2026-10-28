//! Rows of the local board's own tables (`publications`, `board_ops`; DESIGN-v2.md §4). They
//! exist in SQLite only and never reach a server; the values match the schema's CHECKs.

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::StoreResult;
use super::sqlite::SqliteTx;
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
    pub const ALL: [PublicationState; 3] = [
        PublicationState::Publishing,
        PublicationState::Published,
        PublicationState::Unpublishing,
    ];

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
    pub const ALL: [BoardOpKind; 3] = [
        BoardOpKind::Publish,
        BoardOpKind::Unpublish,
        BoardOpKind::Migrate,
    ];

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
    pub const ALL: [BoardOpState; 3] = [
        BoardOpState::Pending,
        BoardOpState::Done,
        BoardOpState::Failed,
    ];

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

/// The local board's own tables, inside a [`SqliteTx`] (beside its shared [`super::Tx`]
/// methods, so one transaction can write an agent row and a publication together).
pub trait LocalTx {
    /// Every publication, or those of one board, by board and shared name.
    fn publications(&self, board: Option<&str>) -> StoreResult<Vec<Publication>>;
    fn publication(&self, board: &str, agent_id: &str) -> StoreResult<Option<Publication>>;
    fn publication_by_mailbox(
        &self,
        board: &str,
        mailbox_id: &str,
    ) -> StoreResult<Option<Publication>>;
    fn publication_by_name(
        &self,
        board: &str,
        shared_name: &str,
    ) -> StoreResult<Option<Publication>>;
    fn insert_publication(&self, p: &Publication) -> StoreResult<()>;
    /// Sets the mailbox, generation and state of one publication.
    fn update_publication(
        &self,
        board: &str,
        agent_id: &str,
        route: Option<(&str, u64)>,
        state: PublicationState,
        at: &str,
    ) -> StoreResult<()>;
    fn delete_publication(&self, board: &str, agent_id: &str) -> StoreResult<bool>;
    fn insert_op(&self, op: &BoardOp) -> StoreResult<()>;
    fn op(&self, op_id: &str) -> StoreResult<Option<BoardOp>>;
    fn finish_op(
        &self,
        op_id: &str,
        state: BoardOpState,
        result: Option<&Value>,
        at: &str,
    ) -> StoreResult<()>;
    /// Pending ops, of one board or all, oldest first.
    fn pending_ops(&self, board: Option<&str>) -> StoreResult<Vec<BoardOp>>;
    fn pending_op_for(&self, board: &str, agent_id: &str) -> StoreResult<Option<BoardOp>>;
    /// The board's migrate op that has not failed (pending or done).
    fn migrate_op(&self, board: &str) -> StoreResult<Option<BoardOp>>;
    /// Deletes the alias's publications and ops; returns how many of each.
    fn forget_board(&self, board: &str) -> StoreResult<(usize, usize)>;
}

const PUB_SELECT: &str = "SELECT board, agent_id, shared_name, mailbox_id, route_generation, state, updated_at FROM publications";
const OP_SELECT: &str = "SELECT op_id, board, kind, agent_id, request_json, state, result_json, created_at, updated_at FROM board_ops";

fn pub_row(r: &rusqlite::Row) -> rusqlite::Result<Publication> {
    let state: String = r.get(5)?;
    let generation: Option<i64> = r.get(4)?;
    Ok(Publication {
        board: r.get(0)?,
        agent_id: r.get(1)?,
        shared_name: r.get(2)?,
        mailbox_id: r.get(3)?,
        route_generation: generation.map(|g| g as u64),
        state: PublicationState::parse(&state).unwrap_or(PublicationState::Unpublishing),
        updated_at: r.get(6)?,
    })
}

fn op_row(r: &rusqlite::Row) -> rusqlite::Result<BoardOp> {
    let kind: String = r.get(2)?;
    let state: String = r.get(5)?;
    let request: String = r.get(4)?;
    let result: Option<String> = r.get(6)?;
    Ok(BoardOp {
        op_id: r.get(0)?,
        board: r.get(1)?,
        kind: BoardOpKind::parse(&kind).unwrap_or(BoardOpKind::Migrate),
        agent_id: r.get(3)?,
        request: serde_json::from_str(&request).unwrap_or(Value::Null),
        state: BoardOpState::parse(&state).unwrap_or(BoardOpState::Failed),
        result: result.and_then(|r| serde_json::from_str(&r).ok()),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

impl LocalTx for SqliteTx<'_> {
    fn publications(&self, board: Option<&str>) -> StoreResult<Vec<Publication>> {
        let run = || -> rusqlite::Result<Vec<Publication>> {
            let mut stmt = self.raw().prepare(&format!(
                "{PUB_SELECT} WHERE ?1 IS NULL OR board = ?1 ORDER BY board, shared_name"
            ))?;
            let rows = stmt.query_map(params![board], pub_row)?;
            rows.collect()
        };
        self.guard(run())
    }

    fn publication(&self, board: &str, agent_id: &str) -> StoreResult<Option<Publication>> {
        let sql = format!("{PUB_SELECT} WHERE board = ?1 AND agent_id = ?2");
        self.guard(
            self.raw()
                .query_row(&sql, params![board, agent_id], pub_row)
                .optional(),
        )
    }

    fn publication_by_mailbox(
        &self,
        board: &str,
        mailbox_id: &str,
    ) -> StoreResult<Option<Publication>> {
        let sql = format!("{PUB_SELECT} WHERE board = ?1 AND mailbox_id = ?2");
        self.guard(
            self.raw()
                .query_row(&sql, params![board, mailbox_id], pub_row)
                .optional(),
        )
    }

    fn publication_by_name(
        &self,
        board: &str,
        shared_name: &str,
    ) -> StoreResult<Option<Publication>> {
        let sql = format!("{PUB_SELECT} WHERE board = ?1 AND shared_name = ?2");
        self.guard(
            self.raw()
                .query_row(&sql, params![board, shared_name], pub_row)
                .optional(),
        )
    }

    fn insert_publication(&self, p: &Publication) -> StoreResult<()> {
        self.guard(self.raw().execute(
            "INSERT INTO publications(board, agent_id, shared_name, mailbox_id, route_generation, state, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                p.board,
                p.agent_id,
                p.shared_name,
                p.mailbox_id,
                p.route_generation.map(|g| g as i64),
                p.state.as_str(),
                p.updated_at
            ],
        ))?;
        Ok(())
    }

    fn update_publication(
        &self,
        board: &str,
        agent_id: &str,
        route: Option<(&str, u64)>,
        state: PublicationState,
        at: &str,
    ) -> StoreResult<()> {
        self.guard(self.raw().execute(
            "UPDATE publications SET mailbox_id = ?3, route_generation = ?4, state = ?5, updated_at = ?6
             WHERE board = ?1 AND agent_id = ?2",
            params![board, agent_id, route.map(|r| r.0), route.map(|r| r.1 as i64), state.as_str(), at],
        ))?;
        Ok(())
    }

    fn delete_publication(&self, board: &str, agent_id: &str) -> StoreResult<bool> {
        let n = self.guard(self.raw().execute(
            "DELETE FROM publications WHERE board = ?1 AND agent_id = ?2",
            params![board, agent_id],
        ))?;
        Ok(n > 0)
    }

    fn insert_op(&self, op: &BoardOp) -> StoreResult<()> {
        self.guard(self.raw().execute(
            "INSERT INTO board_ops(op_id, board, kind, agent_id, request_json, state, result_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                op.op_id,
                op.board,
                op.kind.as_str(),
                op.agent_id,
                op.request.to_string(),
                op.state.as_str(),
                op.result.as_ref().map(Value::to_string),
                op.created_at,
                op.updated_at
            ],
        ))?;
        Ok(())
    }

    fn op(&self, op_id: &str) -> StoreResult<Option<BoardOp>> {
        let sql = format!("{OP_SELECT} WHERE op_id = ?1");
        self.guard(
            self.raw()
                .query_row(&sql, params![op_id], op_row)
                .optional(),
        )
    }

    fn finish_op(
        &self,
        op_id: &str,
        state: BoardOpState,
        result: Option<&Value>,
        at: &str,
    ) -> StoreResult<()> {
        let n = self.guard(self.raw().execute(
            "UPDATE board_ops SET state = ?2, result_json = ?3, updated_at = ?4 WHERE op_id = ?1",
            params![op_id, state.as_str(), result.map(Value::to_string), at],
        ))?;
        // An op that is gone (its board forgotten) refuses the whole transaction.
        if n != 1 {
            return Err(super::StoreError(format!(
                "board op {op_id} no longer exists"
            )));
        }
        Ok(())
    }

    fn pending_ops(&self, board: Option<&str>) -> StoreResult<Vec<BoardOp>> {
        let run = || -> rusqlite::Result<Vec<BoardOp>> {
            let mut stmt = self.raw().prepare(&format!(
                "{OP_SELECT} WHERE state = 'pending' AND (?1 IS NULL OR board = ?1) ORDER BY created_at, op_id"
            ))?;
            let rows = stmt.query_map(params![board], op_row)?;
            rows.collect()
        };
        self.guard(run())
    }

    fn pending_op_for(&self, board: &str, agent_id: &str) -> StoreResult<Option<BoardOp>> {
        let sql = format!("{OP_SELECT} WHERE state = 'pending' AND board = ?1 AND agent_id = ?2");
        self.guard(
            self.raw()
                .query_row(&sql, params![board, agent_id], op_row)
                .optional(),
        )
    }

    fn migrate_op(&self, board: &str) -> StoreResult<Option<BoardOp>> {
        let sql =
            format!("{OP_SELECT} WHERE kind = 'migrate' AND state <> 'failed' AND board = ?1");
        self.guard(
            self.raw()
                .query_row(&sql, params![board], op_row)
                .optional(),
        )
    }

    fn forget_board(&self, board: &str) -> StoreResult<(usize, usize)> {
        let pubs = self.guard(
            self.raw()
                .execute("DELETE FROM publications WHERE board = ?1", params![board]),
        )?;
        let ops = self.guard(
            self.raw()
                .execute("DELETE FROM board_ops WHERE board = ?1", params![board]),
        )?;
        Ok((pubs, ops))
    }
}
