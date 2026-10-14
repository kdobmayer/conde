//! The local transport between the CLI (and `conde mcp`) and the daemon: NDJSON
//! frames over a unix socket (mode 0600 in a 0700 directory, every peer's uid
//! checked), or on Windows over a named pipe, `\\.\pipe\conde-<user>` by default.
//!
//! The Windows peer check, both ends:
//! - the pipe's DACL grants only the current user's SID, and it rejects remote
//!   clients, so another account or another machine cannot open it;
//! - the daemon checks each client's process token user (by the pid the pipe
//!   reports) against its own SID, like the uid check on Unix;
//! - the client checks the serving process's user the same way, because a pipe
//!   name, unlike a socket in a 0700 directory, can be created first by anyone
//!   (a squatter): a client refuses a pipe served by another account;
//! - the daemon creates the pipe as its first instance, so it never joins a
//!   squatter's pipe, and fails at start instead.
//!
//! What it cannot verify: the check is per user, like the uid check, so any process
//! of the same user is trusted; the pid-to-token lookup reads the peer at accept
//! time, so a peer that exits and has its pid reused in between is checked as the
//! new process (no worse than the uid check, which is also per user).

use std::io;
use std::path::Path;

#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::time::{Duration, Instant};

/// How long the Windows client waits for a busy pipe before giving up.
#[cfg(windows)]
const CONNECT_WAIT: Duration = Duration::from_secs(5);

// --- client ------------------------------------------------------------------

/// A blocking client stream.
#[cfg(unix)]
pub type Stream = std::os::unix::net::UnixStream;

/// A blocking client stream over the daemon's pipe; clones share the handle.
#[cfg(windows)]
pub struct Stream {
    pipe: Arc<conde_transport::win::PipeClient>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

/// Connect to the daemon at `path`.
pub fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Stream::connect(path)
    }
    #[cfg(windows)]
    {
        use conde_transport::win;
        let pipe = win::PipeClient::connect(path.as_os_str(), Some(Instant::now() + CONNECT_WAIT))?;
        let own = win::own_user_sid()?;
        let server = pipe.server_pid().and_then(win::process_user_sid);
        crate::util::same_sid(server, &own).map_err(|why| {
            io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is not this user's daemon: {why}", path.display()))
        })?;
        Ok(Stream { pipe: Arc::new(pipe), read_timeout: None, write_timeout: None })
    }
}

#[cfg(windows)]
impl Stream {
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(Stream { pipe: self.pipe.clone(), read_timeout: self.read_timeout, write_timeout: self.write_timeout })
    }

    pub fn set_read_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.read_timeout = t;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.write_timeout = t;
        Ok(())
    }
}

#[cfg(windows)]
impl io::Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pipe.read(buf, self.read_timeout.map(|t| Instant::now() + t))
    }
}

#[cfg(windows)]
impl io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

#[cfg(windows)]
impl io::Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pipe.write_all(buf, self.write_timeout.map(|t| Instant::now() + t)).map(|()| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
impl io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// --- daemon ------------------------------------------------------------------

/// The daemon's listener.
#[cfg(unix)]
pub type Listener = tokio::net::UnixListener;

/// One accepted connection.
#[cfg(unix)]
pub type Conn = tokio::net::UnixStream;

#[cfg(windows)]
pub type Conn = tokio::net::windows::named_pipe::NamedPipeServer;

/// The daemon's pipe: the instance waiting for the next client.
#[cfg(windows)]
pub struct Listener {
    name: std::ffi::OsString,
    next: Conn,
    security: conde_transport::win::Security,
    own_sid: String,
}

#[cfg(windows)]
fn create(name: &std::ffi::OsStr, security: &conde_transport::win::Security, first: bool) -> io::Result<Conn> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: the attributes and their descriptor outlive the call (owned by the caller).
    unsafe { opts.create_with_security_attributes_raw(name, security.attributes().cast_mut().cast()) }
}

/// Create the pipe at `path` (owner-only), refusing to run beside a live daemon.
/// Needs a tokio runtime context.
#[cfg(windows)]
pub fn bind(path: &Path) -> anyhow::Result<Listener> {
    use anyhow::Context;
    use conde_transport::win;
    let security = win::Security::owner_only(false).context("building the pipe's security descriptor")?;
    let name = path.as_os_str().to_owned();
    let next = match create(&name, &security, true) {
        Ok(p) => p,
        // a first instance of an existing pipe is refused with ERROR_ACCESS_DENIED
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            anyhow::bail!("another conde daemon (or another program) is listening on {}", path.display())
        }
        Err(e) => return Err(e).with_context(|| format!("creating the pipe {}", path.display())),
    };
    Ok(Listener { name, next, security, own_sid: win::own_user_sid()? })
}

/// The next connection, and Err(why) when its peer is another user.
pub async fn accept(l: &mut Listener) -> io::Result<(Conn, Result<(), String>)> {
    #[cfg(unix)]
    {
        let (stream, _) = l.accept().await?;
        let check = crate::util::same_user(stream.peer_cred().map(|c| c.uid()), crate::util::own_euid());
        Ok((stream, check))
    }
    #[cfg(windows)]
    {
        l.next.connect().await?;
        let fresh = create(&l.name, &l.security, false)?;
        let conn = std::mem::replace(&mut l.next, fresh);
        let peer = conde_transport::win::client_pid(&conn).and_then(conde_transport::win::process_user_sid);
        let check = crate::util::same_sid(peer, &l.own_sid);
        Ok((conn, check))
    }
}
