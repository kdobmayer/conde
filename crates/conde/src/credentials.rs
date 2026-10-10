//! The host's device credential: `$XDG_CONFIG_HOME/conde/credentials`
//! (mode 0600; on Windows `%APPDATA%\conde\credentials`, owner-only), written by
//! `conde enroll`, read by the bridge at start.

use std::io::{BufRead, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// The server URL given at enrollment (https://, or ws:// in tests).
    pub server: String,
    pub host_id: String,
    pub key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("server", &self.server).field("host_id", &self.host_id).finish_non_exhaustive()
    }
}

/// `$XDG_CONFIG_HOME/conde/credentials`, else `~/.config/conde/credentials`
/// (Windows: `%APPDATA%\conde\credentials`).
pub fn path() -> anyhow::Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        #[cfg(windows)]
        None => PathBuf::from(std::env::var_os("APPDATA").filter(|h| !h.is_empty()).context("APPDATA is not set")?),
        #[cfg(not(windows))]
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME is not set")?)
            .join(".config"),
    };
    Ok(base.join("conde").join("credentials"))
}

/// The file's permission bits, when it exists.
#[cfg(unix)]
pub fn mode(path: &Path) -> Option<u32> {
    std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777)
}

/// Read the credential; refuse a file other users could read.
pub fn load(path: &Path) -> anyhow::Result<Credentials> {
    let raw = std::fs::read(path)
        .with_context(|| format!("no host credential at {} (run: conde enroll --server <url>)", path.display()))?;
    crate::private::check(path)?;
    serde_json::from_slice(&raw).with_context(|| format!("{} is not a conde credential file", path.display()))
}

/// Write the credential atomically with mode 0600 (directory 0700).
pub fn store(path: &Path, creds: &Credentials) -> anyhow::Result<()> {
    let dir = path.parent().context("credential path has no directory")?;
    crate::private::create_dir(dir)?;
    let tmp = dir.join(format!(".credentials.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = crate::private::create_file(&tmp)?;
    f.write_all(serde_json::to_string_pretty(creds)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// (tls, host[:port]) of a server URL; http/ws and https/wss pair up.
fn origin(url: &str) -> anyhow::Result<(bool, String)> {
    let (scheme, rest) = url.split_once("://").with_context(|| format!("'{url}' is not a URL (https://host)"))?;
    let tls = match scheme.to_ascii_lowercase().as_str() {
        "https" | "wss" => true,
        "http" | "ws" => false,
        other => bail!("unsupported scheme '{other}' in '{url}' (https:// or wss://)"),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        bail!("'{url}' has no usable host");
    }
    Ok((tls, authority))
}

/// The bridge WebSocket URL for a server URL: `wss://host/v1/bridge`.
pub fn bridge_url(server: &str) -> anyhow::Result<String> {
    let (tls, authority) = origin(server)?;
    let path = server.split_once("://").map(|(_, r)| r).unwrap_or_default();
    let path = path.find('/').map(|i| &path[i..]).unwrap_or("").trim_end_matches('/');
    let path = if path.is_empty() { "/v1/bridge" } else { path };
    Ok(format!("{}://{authority}{path}", if tls { "wss" } else { "ws" }))
}

/// The RPC URL for a server URL: `https://host/v1/rpc` (`http://` for a plain server).
pub fn rpc_url(server: &str) -> anyhow::Result<String> {
    let (tls, authority) = origin(server)?;
    Ok(format!("{}://{authority}/v1/rpc", if tls { "https" } else { "http" }))
}

/// Whether two server URLs name the same server (the key never goes elsewhere).
pub fn same_server(a: &str, b: &str) -> anyhow::Result<bool> {
    Ok(origin(a)? == origin(b)?)
}

/// Read a secret at a prompt on the terminal with echo off; `no_tty` is the error without one.
#[cfg(windows)]
pub fn prompt_secret(prompt: &str, no_tty: &'static str) -> anyhow::Result<String> {
    crate::win::read_secret(prompt).context(no_tty)
}

/// Read a secret at a prompt on the terminal with echo off; `no_tty` is the error without one.
#[cfg(unix)]
pub fn prompt_secret(prompt: &str, no_tty: &'static str) -> anyhow::Result<String> {
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context(no_tty)?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&tty);
    // SAFETY: tcgetattr/tcsetattr on an open tty fd with a zeroed termios out-parameter.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        bail!("cannot read the terminal settings: {}", std::io::Error::last_os_error());
    }
    let mut quiet = saved;
    quiet.c_lflag &= !libc::ECHO;
    quiet.c_lflag |= libc::ECHONL;
    (&tty).write_all(prompt.as_bytes())?;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    let mut line = String::new();
    let read = std::io::BufReader::new(&tty).read_line(&mut line);
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    read?;
    Ok(line.trim().to_string())
}

/// One line from stdin (`--key-stdin`), for scripted enrollment.
pub fn read_stdin_secret() -> anyhow::Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_urls() {
        assert_eq!(bridge_url("https://conde.nativeai.space").unwrap(), "wss://conde.nativeai.space/v1/bridge");
        assert_eq!(bridge_url("wss://conde.nativeai.space/").unwrap(), "wss://conde.nativeai.space/v1/bridge");
        assert_eq!(bridge_url("ws://127.0.0.1:8080").unwrap(), "ws://127.0.0.1:8080/v1/bridge");
        assert_eq!(bridge_url("ws://h/custom/path").unwrap(), "ws://h/custom/path");
        assert!(bridge_url("ftp://h").is_err());
        assert!(bridge_url("conde.nativeai.space").is_err());
        assert!(same_server("https://Conde.nativeai.space", "wss://conde.nativeai.space/v1/bridge").unwrap());
        assert!(!same_server("https://conde.nativeai.space", "ws://conde.nativeai.space").unwrap());
        assert!(!same_server("https://conde.nativeai.space", "wss://evil.example").unwrap());
    }

    #[test]
    fn store_is_private_and_load_refuses_open_modes() {
        let dir = std::env::temp_dir().join(format!("conde-cred-{}", crate::util::new_ulid()));
        let path = dir.join("conde/credentials");
        let creds = Credentials { server: "https://s".into(), host_id: "h".into(), key: "k".into() };
        store(&path, &creds).unwrap();
        assert_eq!(mode(&path), Some(0o600));
        assert_eq!(mode(path.parent().unwrap()), Some(0o700));
        assert_eq!(load(&path).unwrap().key, "k");
        assert!(!format!("{creds:?}").contains("\"k\""));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("mode 644"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
