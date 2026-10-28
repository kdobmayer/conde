//! Wire frames: one JSON object per frame (NDJSON on the unix socket).

pub mod v2;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
/// Largest accepted frame (attachments travel inline as base64).
pub const MAX_FRAME: usize = 48 << 20;
pub const MAX_TEXT: usize = 256 << 10;
pub const MAX_ATTACHMENT: usize = 8 << 20;
pub const MAX_MESSAGE_ATTACHMENTS: usize = 32 << 20;
/// Page size of inbox / search / receipt listings.
pub const PAGE: usize = 200;
/// Messages one search request scans at most before it answers with what it found and a
/// cursor to continue: a pattern that never matches costs a bounded scan, not all history.
/// The scan reads whole batches of 500, so it stops (and reports `scanned`) at the first batch
/// boundary at or past this.
pub const SEARCH_SCAN: i64 = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    #[serde(rename = "type")]
    pub frame_type: String,
    pub request_id: String,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The caller's harness address as derived by the CLI (`kind:target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Human-only, local-only override: act as this registered name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The board the caller named (`--board <alias>`, MCP `board`), verbatim; the daemon's
    /// resolver decides the board when absent (DESIGN-v2 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(default)]
    pub body: Value,
}

impl Request {
    pub fn new(request_id: impl Into<String>, op: impl Into<String>, body: Value) -> Request {
        Request {
            v: VERSION,
            frame_type: "request".into(),
            request_id: request_id.into(),
            op: op.into(),
            idempotency_key: None,
            actor: None,
            from: None,
            board: None,
            body,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthenticated,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    #[serde(rename = "type")]
    pub frame_type: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(request_id: &str, result: Value) -> Response {
        Response {
            v: VERSION,
            frame_type: "response".into(),
            request_id: request_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(request_id: &str, code: ErrorCode, message: impl Into<String>) -> Response {
        Response {
            v: VERSION,
            frame_type: "response".into(),
            request_id: request_id.into(),
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}
