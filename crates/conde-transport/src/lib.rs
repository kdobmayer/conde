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
//!
//! On Windows the Claude Code inbox is a named pipe (`\\.\pipe\LOCAL\cc-msg-<hex>`)
//! and the Codex control endpoint an AF_UNIX socket file; [`win`] holds the OS
//! primitives both legs and the conde CLI share there.

mod claude;
mod codex;
#[cfg(windows)]
pub mod win;

pub use claude::{
    ClaudeSender, ClaudeSession, Receipt, ReceiptStatus, discover_claude, msg_id_for,
    resolve_claude, uds_address,
};
pub use codex::{
    CodexOutcome, PROVENANCE, codex_control_present, codex_control_socket, codex_home,
    codex_notify, codex_notify_with_id,
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

/// `$VAR` when set and non-empty, else `<home>/<fallback>`.
pub(crate) fn env_dir_or_home(var: &str, fallback: &str) -> Option<std::path::PathBuf> {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => Some(v.into()),
        _ => home_dir().map(|h| h.join(fallback)),
    }
}

/// The user's home: `$HOME`; on Windows `%USERPROFILE%`, where Claude Code and
/// Codex keep `.claude` and `.codex`.
pub fn home_dir() -> Option<std::path::PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
}

/// SDDL for an object only `sid` may access: a protected DACL with one
/// generic-all allow ACE, inherited by children when `container`.
pub fn owner_only_sddl(sid: &str, container: bool) -> String {
    let inherit = if container { "OICI" } else { "" };
    format!("D:P(A;{inherit};GA;;;{sid})")
}

#[cfg(test)]
mod tests {
    #[test]
    fn owner_only_sddl_grants_one_sid() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            super::owner_only_sddl(sid, false),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
        );
        assert_eq!(
            super::owner_only_sddl(sid, true),
            "D:P(A;OICI;GA;;;S-1-5-21-1-2-3-1001)"
        );
    }
}
