//! Where the board and the socket live.

use std::path::PathBuf;

/// `$CONDE_DATA`, else `~/.local/share/conde` (Windows: `%LOCALAPPDATA%\conde\data`).
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CONDE_DATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        windows_data_dir(
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            conde_transport::home_dir(),
        )
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".local/share/conde")
    }
}

/// `%LOCALAPPDATA%\conde\data`, else the same under `<home>\AppData\Local`.
pub fn windows_data_dir(local_appdata: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    local_appdata
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| {
            home.unwrap_or_else(|| PathBuf::from("."))
                .join("AppData")
                .join("Local")
        })
        .join("conde")
        .join("data")
}

/// `$CONDE_SOCK`, else `$XDG_RUNTIME_DIR/conde/conde.sock` (Windows: the pipe
/// `\\.\pipe\conde-<user>`).
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("CONDE_SOCK").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    default_socket_path()
}

/// The socket without `$CONDE_SOCK`: `$XDG_RUNTIME_DIR/conde/conde.sock`.
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("conde-{}", crate::util::own_uid())));
    runtime.join("conde").join("conde.sock")
}

/// The socket without `$CONDE_SOCK`: the named pipe `\\.\pipe\conde-<user>`.
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    PathBuf::from(pipe_for_user(&crate::util::local_user()))
}

/// `\\.\pipe\conde-<user>`, the user name reduced to `[a-z0-9._-]`.
pub fn pipe_for_user(user: &str) -> String {
    let name: String = user
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!(r"\\.\pipe\conde-{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_locations() {
        assert_eq!(pipe_for_user("Krisz"), r"\\.\pipe\conde-krisz");
        assert_eq!(pipe_for_user("Kovács Béla"), r"\\.\pipe\conde-kov-cs-b-la");
        assert_eq!(pipe_for_user(r"DOMAIN\bob"), r"\\.\pipe\conde-domain-bob");
        assert_eq!(
            windows_data_dir(Some(PathBuf::from("L")), Some(PathBuf::from("H"))),
            PathBuf::from("L").join("conde").join("data")
        );
        assert_eq!(
            windows_data_dir(None, Some(PathBuf::from("H"))),
            PathBuf::from("H")
                .join("AppData")
                .join("Local")
                .join("conde")
                .join("data")
        );
    }
}
