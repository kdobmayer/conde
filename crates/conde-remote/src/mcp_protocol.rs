//! What the stdio MCP server (`conde mcp`) and the remote MCP endpoint agree on: the protocol
//! revisions served and the JSON-RPC error codes, so the HTTP transport can validate headers
//! and map errors to statuses without the tool layer.

/// Served statelessly with per-request `_meta`.
pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
/// Accepted through an `initialize` handshake (first is the fallback offer).
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The revision of a legacy HTTP request without an `MCP-Protocol-Version` header.
pub const HEADERLESS_VERSION: &str = "2025-03-26";

pub const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const HEADER_MISMATCH: i64 = -32020;
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Every revision served, modern first.
pub fn supported_versions() -> Vec<&'static str> {
    MODERN_VERSIONS.iter().chain(LEGACY_VERSIONS).copied().collect()
}

/// A JSON-RPC error response.
pub fn error_response(id: &serde_json::Value, code: i64, message: &str, data: Option<serde_json::Value>) -> serde_json::Value {
    let mut error = serde_json::json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error})
}
