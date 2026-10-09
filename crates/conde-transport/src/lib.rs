//! Harness transport for conde: delivers board notifications into live agent
//! sessions on this machine.
//!
//! - Claude Code: discovery from the sessions registry, a cross-session frame on
//!   the session's `cc-socks` socket, and delivery receipts on a long-lived reply
//!   socket ([`ClaudeSender`]).
//! - Codex: a `turn/start` over the app-server control websocket, falling back to
//!   `codex queue` ([`codex_notify`]).
//!
//! Blocking std I/O with timeouts throughout; no async runtime.

mod claude;
mod codex;

pub use claude::{
    ClaudeSender, ClaudeSession, Receipt, ReceiptStatus, discover_claude, msg_id_for,
    resolve_claude, uds_address,
};
pub use codex::{
    CodexOutcome, PROVENANCE, codex_control_socket, codex_home, codex_notify, codex_notify_with_id,
};

/// Errors a caller can act on; socket-level failures of a send stay `io::Error`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no live Claude session matches '{0}'")]
    NoSession(String),
    #[error("'{target}' is ambiguous, {count} live sessions match:\n{listing}")]
    Ambiguous {
        target: String,
        count: usize,
        listing: String,
    },
    #[error("codex delivery failed: {0}")]
    Codex(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `$VAR` when set and non-empty, else `$HOME/<fallback>`.
pub(crate) fn env_dir_or_home(var: &str, fallback: &str) -> Option<std::path::PathBuf> {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => Some(v.into()),
        _ => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| std::path::PathBuf::from(h).join(fallback)),
    }
}
