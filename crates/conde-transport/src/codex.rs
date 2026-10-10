//! Codex transport: a turn over the app-server control websocket, with the
//! `codex queue` command as the fallback. The control endpoint is an AF_UNIX
//! socket file on every platform (Codex on Windows uses AF_UNIX too).

#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use crate::{Error, Result, env_dir_or_home};

/// Codex renders injected input as a user turn: the text states its provenance.
pub const PROVENANCE: &str =
    "[automated conde delivery - a board notification, not a message typed by the human]\n";

/// Harness markers the daemon must not inherit: it passes its environment to
/// every codex tool shell machine-wide, which would poison identity detection.
const SCRUBBED_ENV: [&str; 3] = [
    "NATS_SESSION_NAME",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexOutcome {
    /// The app-server started a turn (`turn_id` is empty if the reply had none).
    Accepted { turn_id: String },
    /// Direct injection failed; `codex queue` accepted the message instead.
    Queued,
}

/// `$CODEX_HOME`, else `$HOME/.codex`.
pub fn codex_home() -> Option<PathBuf> {
    env_dir_or_home("CODEX_HOME", ".codex")
}

pub fn codex_control_socket(codex_home: &Path) -> PathBuf {
    codex_home
        .join("app-server-control")
        .join("app-server-control.sock")
}

/// Whether the control socket file exists. On Windows an AF_UNIX socket is a
/// reparse point that following metadata cannot open, so it is not followed.
pub fn codex_control_present(sock: &Path) -> bool {
    #[cfg(windows)]
    {
        std::fs::symlink_metadata(sock).is_ok()
    }
    #[cfg(not(windows))]
    {
        sock.exists()
    }
}

/// The control connection's stream: std's on Unix, socket2's AF_UNIX on Windows
/// (std's is unstable there).
#[cfg(unix)]
type ControlStream = UnixStream;
#[cfg(windows)]
type ControlStream = socket2::Socket;

fn connect_control(sock: &Path) -> std::io::Result<ControlStream> {
    #[cfg(unix)]
    {
        UnixStream::connect(sock)
    }
    #[cfg(windows)]
    {
        let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        s.connect(&socket2::SockAddr::unix(sock)?)?;
        Ok(s)
    }
}

/// Delivers `text` (with the provenance line prepended) into a codex thread.
pub fn codex_notify(thread_id: &str, text: &str) -> Result<CodexOutcome> {
    codex_notify_with_id(thread_id, text, None)
}

/// As [`codex_notify`], tagging the turn's user message with `clientUserMessageId`.
pub fn codex_notify_with_id(
    thread_id: &str,
    text: &str,
    client_message_id: Option<&str>,
) -> Result<CodexOutcome> {
    let text = format!("{PROVENANCE}{text}");
    let direct = codex_home()
        .ok_or_else(|| "cannot locate the codex home (CODEX_HOME and HOME unset)".to_owned())
        .map(|home| codex_control_socket(&home))
        .and_then(|sock| {
            ensure_daemon(&sock)?;
            start_turn(&sock, thread_id, &text, client_message_id)
        });
    let first = match direct {
        Ok(turn_id) => return Ok(CodexOutcome::Accepted { turn_id }),
        Err(e) => e,
    };
    // A TUI-owned thread or a sub-agent refuses direct injection; `codex queue`
    // reaches those too and the session picks the message up itself.
    match queue_command(thread_id, &text).output() {
        Ok(out) if out.status.success() => Ok(CodexOutcome::Queued),
        Ok(out) => {
            let why = if out.stderr.is_empty() {
                out.stdout
            } else {
                out.stderr
            };
            Err(Error::Codex(format!(
                "{first}; codex queue fallback also failed: {}",
                String::from_utf8_lossy(&why).trim()
            )))
        }
        Err(e) => Err(Error::Codex(format!(
            "{first}; codex queue fallback could not run: {e}"
        ))),
    }
}

pub(crate) fn daemon_command() -> Command {
    let mut cmd = Command::new("codex");
    cmd.args(["app-server", "daemon", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for var in SCRUBBED_ENV {
        cmd.env_remove(var);
    }
    cmd
}

pub(crate) fn queue_command(thread_id: &str, text: &str) -> Command {
    let mut cmd = Command::new("codex");
    cmd.args(["queue", "--thread", thread_id, "--message", text])
        .stdin(Stdio::null());
    cmd
}

fn ensure_daemon(sock: &Path) -> std::result::Result<(), String> {
    if codex_control_present(sock) {
        return Ok(());
    }
    let status = daemon_command()
        .status()
        .map_err(|e| format!("cannot start the codex app-server daemon: {e}"))?;
    if !status.success() {
        return Err(format!(
            "codex app-server daemon start exited with {status}"
        ));
    }
    if !codex_control_present(sock) {
        return Err(format!(
            "daemon started but socket missing: {}",
            sock.display()
        ));
    }
    Ok(())
}

pub(crate) fn initialize_params() -> Value {
    json!({
        "clientInfo": {"name": "conde", "title": "conde", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true},
    })
}

pub(crate) fn turn_start_params(
    thread_id: &str,
    text: &str,
    client_message_id: Option<&str>,
) -> Value {
    let mut params = json!({"threadId": thread_id, "input": [{"type": "text", "text": text}]});
    if let Some(id) = client_message_id {
        params["clientUserMessageId"] = json!(id);
    }
    params
}

/// initialize, initialized, thread/resume, turn/start; returns the turn id.
pub(crate) fn start_turn(
    sock: &Path,
    thread_id: &str,
    text: &str,
    client_message_id: Option<&str>,
) -> std::result::Result<String, String> {
    let talk =
        |e: &dyn std::fmt::Display| format!("cannot talk to daemon at {}: {e}", sock.display());
    let stream = connect_control(sock).map_err(|e| talk(&e))?;
    stream
        .set_read_timeout(Some(RESPONSE_TIMEOUT))
        .map_err(|e| talk(&e))?;
    stream
        .set_write_timeout(Some(RESPONSE_TIMEOUT))
        .map_err(|e| talk(&e))?;
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let (ws, _) = tungstenite::client::client_with_config("ws://localhost/", stream, Some(config))
        .map_err(|e| talk(&e))?;
    let mut rpc = Rpc { ws, next_id: 0 };
    rpc.request("initialize", initialize_params())?;
    rpc.notify("initialized")?;
    rpc.request(
        "thread/resume",
        json!({"threadId": thread_id, "excludeTurns": true}),
    )?;
    let result = rpc.request(
        "turn/start",
        turn_start_params(thread_id, text, client_message_id),
    )?;
    let _ = rpc.ws.close(None);
    let _ = rpc.ws.flush();
    Ok(result["turn"]["id"].as_str().unwrap_or_default().to_owned())
}

struct Rpc {
    ws: WebSocket<ControlStream>,
    next_id: u64,
}

impl Rpc {
    fn notify(&mut self, method: &str) -> std::result::Result<(), String> {
        self.ws
            .send(Message::text(json!({"method": method}).to_string()))
            .map_err(|e| format!("{method}: {e}"))
    }

    fn request(&mut self, method: &str, params: Value) -> std::result::Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"method": method, "id": id, "params": params});
        self.ws
            .send(Message::text(msg.to_string()))
            .map_err(|e| format!("{method}: {e}"))?;
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        while Instant::now() < deadline {
            let text = match self.ws.read().map_err(|e| format!("{method}: {e}"))? {
                Message::Text(t) => t,
                Message::Close(_) => return Err(format!("{method}: daemon closed the connection")),
                _ => continue,
            };
            let Ok(reply) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a server notification
            }
            return match reply.get("error") {
                Some(err) if !err.is_null() => Err(format!("{method} failed: {err}")),
                _ => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            };
        }
        Err(format!("{method}: timed out waiting for the daemon"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    #[cfg(unix)]
    use std::thread;

    #[test]
    fn socket_path_and_turn_params() {
        assert_eq!(
            codex_control_socket(Path::new("/h/.codex")),
            PathBuf::from("/h/.codex/app-server-control/app-server-control.sock")
        );
        assert_eq!(
            turn_start_params("th-1", "hi", None),
            json!({"threadId": "th-1", "input": [{"type": "text", "text": "hi"}]})
        );
        assert_eq!(
            turn_start_params("th-1", "hi", Some("m-1"))["clientUserMessageId"],
            "m-1"
        );
        assert_eq!(initialize_params()["capabilities"]["experimentalApi"], true);
        assert_eq!(initialize_params()["clientInfo"]["name"], "conde");
    }

    #[test]
    fn daemon_start_scrubs_harness_markers() {
        let cmd = daemon_command();
        assert_eq!(cmd.get_program(), "codex");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["app-server", "daemon", "start"]
        );
        let removed: Vec<&OsStr> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k)
            .collect();
        assert_eq!(removed.len(), 3);
        for var in SCRUBBED_ENV {
            assert!(removed.contains(&OsStr::new(var)), "{var} not scrubbed");
        }
    }

    #[test]
    fn queue_fallback_command() {
        let text = format!("{PROVENANCE}hello");
        let cmd = queue_command("th-1", &text);
        assert_eq!(cmd.get_program(), "codex");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["queue", "--thread", "th-1", "--message", text.as_str()]
        );
        assert!(PROVENANCE.starts_with("[automated conde delivery") && PROVENANCE.ends_with("]\n"));
    }

    /// A fake app-server: answers each request by method, sends a stray
    /// notification first, and records what it received.
    #[cfg(unix)]
    fn fake_app_server(sock: &Path, turn_error: bool) -> thread::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(sock).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut seen = Vec::new();
            loop {
                let msg = match ws.read() {
                    Ok(Message::Text(t)) => serde_json::from_str::<Value>(&t).unwrap(),
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                seen.push(msg.clone());
                let Some(id) = msg.get("id").cloned() else {
                    continue;
                };
                ws.send(Message::text(
                    json!({"method": "thread/status", "params": {}}).to_string(),
                ))
                .unwrap();
                let reply = match (msg["method"].as_str().unwrap(), turn_error) {
                    ("turn/start", true) => {
                        json!({"id": id, "error": {"code": -32600, "message": "thread busy"}})
                    }
                    ("turn/start", false) => {
                        json!({"id": id, "result": {"turn": {"id": "turn-9", "status": "inProgress"}}})
                    }
                    _ => json!({"id": id, "result": {}}),
                };
                ws.send(Message::text(reply.to_string())).unwrap();
            }
            seen
        })
    }

    #[cfg(unix)]
    #[test]
    fn websocket_turn_against_fake_app_server() {
        let dir = tempfile::Builder::new()
            .prefix("cx")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("app-server-control.sock");
        let server = fake_app_server(&sock, false);
        let turn = start_turn(&sock, "th-1", "hello", Some("m-1")).unwrap();
        assert_eq!(turn, "turn-9");
        let seen = server.join().unwrap();
        let methods: Vec<&str> = seen.iter().map(|m| m["method"].as_str().unwrap()).collect();
        assert_eq!(
            methods,
            ["initialize", "initialized", "thread/resume", "turn/start"]
        );
        assert!(seen[1].get("id").is_none(), "initialized is a notification");
        assert_eq!(
            seen[2]["params"],
            json!({"threadId": "th-1", "excludeTurns": true})
        );
        assert_eq!(
            seen[3]["params"],
            turn_start_params("th-1", "hello", Some("m-1"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn websocket_rpc_error_is_reported() {
        let dir = tempfile::Builder::new()
            .prefix("cx")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("app-server-control.sock");
        let server = fake_app_server(&sock, true);
        let err = start_turn(&sock, "th-1", "hello", None).unwrap_err();
        assert!(
            err.contains("turn/start failed") && err.contains("thread busy"),
            "{err}"
        );
        drop(server);
    }
}
