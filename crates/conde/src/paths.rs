//! Where the board and the socket live.

use std::path::PathBuf;

/// `$CONDE_DATA`, else `~/.local/share/conde`.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CONDE_DATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/conde")
}

/// `$CONDE_SOCK`, else `$XDG_RUNTIME_DIR/conde/conde.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("CONDE_SOCK").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    default_socket_path()
}

/// The socket without `$CONDE_SOCK`: `$XDG_RUNTIME_DIR/conde/conde.sock`.
pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("conde-{}", crate::util::own_uid())));
    runtime.join("conde").join("conde.sock")
}
