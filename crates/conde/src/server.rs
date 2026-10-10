//! Unix-socket listener: NDJSON request frames in, response frames out.
//! Local mode trusts the socket: only the owning OS user may connect.
//! (Windows binds a named pipe instead, see [`crate::ipc`]; the frames are shared.)

use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use anyhow::{Context, bail};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::io::BufReader;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(unix)]
use crate::dispatch::{Dispatcher, Principal};
#[cfg(unix)]
use crate::protocol::{ErrorCode, MAX_FRAME, Request};
use crate::protocol::Response;
#[cfg(unix)]
use crate::util::{own_euid, same_user};

/// Bind the socket (mode 0600, parent 0700), replacing a stale one but never
/// a live daemon's.
#[cfg(unix)]
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!("another conde daemon is listening on {}", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(unix)]
pub async fn serve(listener: UnixListener, dispatcher: Arc<Dispatcher>, principal: Principal) -> anyhow::Result<()> {
    let uid = own_euid();
    loop {
        let (stream, _) = listener.accept().await?;
        if let Err(why) = same_user(stream.peer_cred().map(|c| c.uid()), uid) {
            tracing::warn!("{why}");
            continue;
        }
        tokio::spawn(connection(stream, dispatcher.clone(), principal.clone()));
    }
}

#[cfg(unix)]
async fn connection(stream: UnixStream, dispatcher: Arc<Dispatcher>, principal: Principal) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let response = match read_frame(&mut reader, MAX_FRAME).await {
            Ok(None) => return,
            Ok(Some(frame)) if frame.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Some(frame)) => match serde_json::from_slice::<Request>(&frame) {
                Ok(req) => {
                    let (d, p) = (dispatcher.clone(), principal.clone());
                    tokio::task::spawn_blocking(move || d.dispatch(&p, &req))
                        .await
                        .unwrap_or_else(|e| Response::err("", ErrorCode::Internal, format!("dispatch panicked: {e}")))
                }
                Err(e) => Response::err("", ErrorCode::BadRequest, format!("bad frame: {e}")),
            },
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let resp = Response::err("", ErrorCode::TooLarge, format!("frame exceeds {MAX_FRAME} bytes"));
                let _ = write_frame(&mut write, &resp).await;
                return;
            }
            Err(_) => return,
        };
        if write_frame(&mut write, &response).await.is_err() {
            return;
        }
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), resp: &Response) -> io::Result<()> {
    let mut line = serde_json::to_vec(resp).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await
}

/// One newline-terminated frame, or None at EOF; InvalidData past `max`.
pub async fn read_frame(r: &mut (impl AsyncBufRead + Unpin), max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            return Ok((!buf.is_empty()).then_some(buf));
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let take = newline.unwrap_or(available.len());
        buf.extend_from_slice(&available[..take]);
        r.consume(newline.map_or(take, |pos| pos + 1));
        if buf.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        if newline.is_some() {
            return Ok(Some(buf));
        }
    }
}
