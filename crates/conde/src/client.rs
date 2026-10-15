//! Blocking client used by the CLI, over the local transport ([`crate::ipc`]).

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::protocol::{Request, Response};

use crate::ipc::Stream;

/// How long one request waits for the daemon's answer. Above the daemon's own 30 s limit on a
/// shared board's call, so that error, which names the board, arrives first.
pub const CALL_DEADLINE: Duration = Duration::from_secs(60);

pub struct Client {
    reader: BufReader<Stream>,
    writer: Stream,
    path: PathBuf,
    deadline: Duration,
}

impl Client {
    pub fn connect(path: &Path) -> anyhow::Result<Client> {
        let stream = crate::ipc::connect(path).with_context(|| {
            format!("cannot reach the conde daemon at {} (start it with: conde daemon)", path.display())
        })?;
        Ok(Client {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            path: path.to_path_buf(),
            deadline: CALL_DEADLINE,
        })
    }

    /// The same client with another per-request deadline.
    pub fn with_deadline(mut self, deadline: Duration) -> Client {
        self.deadline = deadline;
        self
    }

    /// One request and its response, within the deadline.
    pub fn call(&mut self, req: &Request) -> anyhow::Result<Response> {
        let deadline = Instant::now() + self.deadline;
        let late = || {
            anyhow::anyhow!(
                "the conde daemon at {} did not answer {} within {} s; it may be stuck: restart the bridge \
                 (see Troubleshooting in README.md)",
                self.path.display(),
                req.op,
                self.deadline.as_secs_f64()
            )
        };
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        self.writer.set_write_timeout(Some(self.deadline))?;
        match self.writer.write_all(&line) {
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Err(late()),
            r => r?,
        }
        let mut buf = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(late());
            }
            self.reader.get_mut().set_read_timeout(Some(left))?;
            match self.reader.read_until(b'\n', &mut buf) {
                Ok(_) if buf.ends_with(b"\n") => break,
                // end of stream before a whole frame
                Ok(_) => bail!("the conde daemon closed the connection"),
                // the bytes read so far stay in buf; the loop checks the deadline
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => return Err(e.into()),
            }
        }
        serde_json::from_slice(&buf).context("unreadable response frame")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A daemon socket that accepts and never answers (or sends half a frame).
    fn silent(dir: &Path, partial: bool) -> PathBuf {
        let path = dir.join("silent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut conn in listener.incoming().flatten() {
                if partial {
                    let _ = conn.write_all(b"{\"v\":1,");
                }
                held.push(conn);
            }
        });
        path
    }

    #[test]
    fn a_daemon_that_does_not_answer_is_an_error_not_a_hang() {
        for partial in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = silent(dir.path(), partial);
            let mut client = Client::connect(&path).unwrap().with_deadline(Duration::from_millis(300));
            let started = Instant::now();
            let err = client.call(&Request::new("r1", "agent.whoami", serde_json::json!({}))).unwrap_err().to_string();
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
            assert!(
                err.starts_with(&format!("the conde daemon at {} did not answer agent.whoami within 0.3 s", path.display())),
                "{err}"
            );
        }
        assert_eq!(CALL_DEADLINE, Duration::from_secs(60));
    }
}
