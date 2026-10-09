//! Claude Code transport: discovery, the cross-session frame, receipts.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Error, Result, env_dir_or_home, sha256_hex};

const ENVELOPE: &str = "cross-session-message";
const FROM_NAME: &str = "conde";
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const RECEIPT_READ_TIMEOUT: Duration = Duration::from_secs(1);
const RECEIPT_MAX_BYTES: usize = 1 << 20;

/// A live Claude Code session as published in `$CLAUDE_CONFIG_DIR/sessions/<pid>.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSession {
    pub name: String,
    pub session_id: String,
    pub pid: u32,
    pub sock: PathBuf,
    pub status: String,
    pub cwd: PathBuf,
    /// The recipient's peer-token file `<pid>.<sha256hex(sock)>.key`; read at send time.
    pub key_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptStatus {
    Held,
    Delivered,
    Expired,
    Denied,
    Refused,
    Dropped,
    /// A status this crate does not know yet, kept verbatim.
    Other(String),
}

impl ReceiptStatus {
    /// Normalizes the wire form: `expired` with detail `refused` is a refusal.
    fn from_wire(status: &str, detail: Option<&str>) -> Self {
        match (status, detail) {
            ("expired", Some("refused")) | ("refused", _) => Self::Refused,
            ("held", _) => Self::Held,
            ("delivered", _) => Self::Delivered,
            ("expired", _) => Self::Expired,
            ("denied", _) => Self::Denied,
            ("dropped", _) => Self::Dropped,
            (other, _) => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Held => "held",
            Self::Delivered => "delivered",
            Self::Expired => "expired",
            Self::Denied => "denied",
            Self::Refused => "refused",
            Self::Dropped => "dropped",
            Self::Other(s) => s,
        }
    }
}

/// A `peer_message_status` control frame sent back by a recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The `msg_id` of the frame this receipt is about, exactly as sent.
    pub msg_id: String,
    pub status: ReceiptStatus,
    pub drop_reason: Option<String>,
    pub status_detail: Option<String>,
}

/// Stable 32-hex id for a key (first 32 hex chars of its sha256).
pub fn msg_id_for(key: &str) -> String {
    sha256_hex(key.as_bytes())[..32].to_owned()
}

/// The harness's address form: `uds:<path>`, every byte outside
/// `[A-Za-z0-9:_/.\-]` percent-encoded as `%XX`.
pub fn uds_address(path: &Path) -> String {
    let mut out = String::from("uds:");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b":_/.\\-".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Start-time fingerprint of `pid` in the form Claude Code publishes as
/// `procStart`, or None if the process is gone or unreadable. Linux: field
/// 22 of /proc/<pid>/stat. macOS: `LC_ALL=C TZ=UTC ps -o lstart=`, rebuilt
/// from `proc_pidinfo(PROC_PIDTBSDINFO).pbi_start_tvsec`.
#[cfg(target_os = "linux")]
pub(crate) fn proc_start(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    rest.split_whitespace().nth(19).map(str::to_owned)
}

#[cfg(target_os = "macos")]
pub(crate) fn proc_start(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is plain integers; the kernel fills `size` bytes.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast::<libc::c_void>(), size)
    };
    (got == size).then(|| lstart_utc(info.pbi_start_tvsec as i64))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn proc_start(_pid: u32) -> Option<String> {
    None
}

/// `ps -o lstart=` under `LC_ALL=C TZ=UTC`: strftime `%c`, i.e.
/// `%a %b %e %H:%M:%S %Y`.
#[cfg(any(target_os = "macos", test))]
fn lstart_utc(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days -> civil date.
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{} {} {d:>2} {:02}:{:02}:{:02} {y}",
        DAYS[(days + 4).rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Published and observed start times agree, ignoring whitespace runs (ps
/// pads the day of month).
fn same_start(published: &str, observed: &str) -> bool {
    published.split_whitespace().eq(observed.split_whitespace())
}

fn sessions_dir(config_dir: Option<&Path>) -> Option<PathBuf> {
    match config_dir {
        Some(d) => Some(d.join("sessions")),
        None => env_dir_or_home("CLAUDE_CONFIG_DIR", ".claude").map(|d| d.join("sessions")),
    }
}

/// Live Claude sessions: the pid exists, its start time matches the published
/// `procStart` (pid not reused), and the messaging socket exists.
/// `config_dir` defaults to `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
pub fn discover_claude(config_dir: Option<&Path>) -> Vec<ClaudeSession> {
    let Some(dir) = sessions_dir(config_dir) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<ClaudeSession> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| fs::read(&p).ok())
        .filter_map(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .filter_map(|info| live_session(&dir, &info))
        .collect();
    out.sort_by_key(|s| s.pid);
    out
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn text_field(info: &Value, key: &str) -> String {
    info.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn live_session(dir: &Path, info: &Value) -> Option<ClaudeSession> {
    let pid = u32::try_from(info.get("pid")?.as_u64()?)
        .ok()
        .filter(|p| *p > 0)?;
    let sock = info
        .get("messagingSocketPath")?
        .as_str()
        .filter(|s| !s.is_empty())?;
    let session_id = scalar(info.get("sessionId")?).filter(|s| !s.is_empty())?;
    let start = proc_start(pid)?;
    // Same rule as kond: an entry without procStart is trusted on the pid alone.
    if let Some(published) = info
        .get("procStart")
        .and_then(scalar)
        .filter(|s| !s.is_empty())
        && !same_start(&published, &start)
    {
        return None;
    }
    if !Path::new(sock).exists() {
        return None;
    }
    Some(ClaudeSession {
        name: text_field(info, "name"),
        session_id,
        pid,
        sock: PathBuf::from(sock),
        status: text_field(info, "status"),
        cwd: PathBuf::from(text_field(info, "cwd")),
        key_path: dir.join(format!("{pid}.{}.key", sha256_hex(sock.as_bytes()))),
    })
}

/// The one session whose session id or name equals `target`.
pub fn resolve_claude<'a>(
    sessions: &'a [ClaudeSession],
    target: &str,
) -> Result<&'a ClaudeSession> {
    if target.is_empty() {
        return Err(Error::NoSession(String::new()));
    }
    let matches: Vec<&ClaudeSession> = sessions
        .iter()
        .filter(|s| s.session_id == target || s.name == target)
        .collect();
    match matches.as_slice() {
        [] => Err(Error::NoSession(target.to_owned())),
        [one] => Ok(one),
        many => Err(Error::Ambiguous {
            target: target.to_owned(),
            count: many.len(),
            listing: many
                .iter()
                .map(|s| format!("- {} session_id={} pid={}", s.name, s.session_id, s.pid))
                .collect::<Vec<_>>()
                .join("\n"),
        }),
    }
}

/// The cross-session envelope. It never carries `from-mode`: conde asserts no
/// sender permission mode, so the recipient's own policy (`crossSessionInbound`)
/// decides whether a peer message is accepted or held.
pub(crate) fn envelope(text: &str, sender: &str) -> String {
    let body = text.replace(&format!("</{ENVELOPE}>"), "");
    format!("<{ENVELOPE} from=\"{sender}\" from-name=\"{FROM_NAME}\">\n{body}\n</{ENVELOPE}>")
}

fn peer_token(key_path: &Path) -> Option<String> {
    let raw = fs::read(key_path).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    v.get("peerToken")?.as_str().map(str::to_owned)
}

/// The bytes written to the recipient: an optional auth line, then the user frame.
pub(crate) fn frame_payload(s: &ClaudeSession, text: &str, msg_id: &str, from: &str) -> String {
    let frame = json!({
        "type": "user",
        "message": {"role": "user", "content": envelope(text, from)},
        "session_id": s.session_id,
        "priority": "next",
        "msg_id": msg_id,
        "from": from,
    });
    let mut out = String::new();
    if let Some(token) = peer_token(&s.key_path) {
        out.push_str(&json!({"type": "auth", "token": token}).to_string());
        out.push('\n');
    }
    out.push_str(&frame.to_string());
    out.push('\n');
    out
}

/// A peer identity for this process: the reply socket `<ownpid>.sock` in the
/// recipients' socket directory, kept for the sender's lifetime.
///
/// The recipient checks the `from` address against the connecting pid and the
/// directory, and sends receipts to it, possibly long after the send, so a
/// long-lived owner (the bridge) must hold one sender for all its sends.
pub struct ClaudeSender {
    path: PathBuf,
    address: String,
    listener: UnixListener,
    inode: (u64, u64),
}

impl ClaudeSender {
    /// Binds `<sock_dir>/<ownpid>.sock` (0600), replacing a stale socket of that name.
    pub fn new(sock_dir: &Path) -> io::Result<Self> {
        let path = sock_dir.join(format!("{}.sock", std::process::id()));
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_socket() => fs::remove_file(&path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        let setup = (|| {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            let m = fs::symlink_metadata(&path)?;
            Ok::<_, io::Error>((m.dev(), m.ino()))
        })();
        let inode = match setup {
            Ok(i) => i,
            Err(e) => {
                let _ = fs::remove_file(&path);
                return Err(e);
            }
        };
        let address = uds_address(&path);
        Ok(Self {
            path,
            address,
            listener,
            inode,
        })
    }

    /// This sender's `uds:` address, the `from` of every frame it sends.
    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one frame to the session's socket. Immediate acceptance sends no
    /// receipt; any receipt for `msg_id` arrives later via [`Self::receipts`].
    pub fn send(&self, s: &ClaudeSession, text: &str, msg_id: &str) -> io::Result<()> {
        let payload = frame_payload(s, text, msg_id, &self.address);
        let sock = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        sock.connect_timeout(&socket2::SockAddr::unix(&s.sock)?, SEND_TIMEOUT)?;
        sock.set_write_timeout(Some(SEND_TIMEOUT))?;
        let mut stream = UnixStream::from(std::os::fd::OwnedFd::from(sock));
        stream.write_all(payload.as_bytes())?;
        stream.flush()
    }

    /// Receipts that arrived: waits up to `wait` for the first connection,
    /// then drains whatever else is pending without waiting.
    pub fn receipts(&self, wait: Duration) -> Vec<Receipt> {
        let mut out = Vec::new();
        let mut deadline = Instant::now() + wait;
        loop {
            if !wait_readable(&self.listener, deadline) {
                return out;
            }
            deadline = Instant::now();
            match self.listener.accept() {
                Ok((conn, _)) => out.extend(read_receipts(conn)),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return out,
            }
        }
    }
}

impl Drop for ClaudeSender {
    fn drop(&mut self) {
        // Unlink only our own socket, not one a later sender bound at the same path.
        if let Ok(m) = fs::symlink_metadata(&self.path)
            && (m.dev(), m.ino()) == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// poll(2) for an incoming connection until `deadline` (a past deadline polls once).
fn wait_readable(listener: &UnixListener, deadline: Instant) -> bool {
    let mut pfd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = left.as_millis().min(i32::MAX as u128) as libc::c_int;
        let ms = if ms == 0 && !left.is_zero() { 1 } else { ms };
        // SAFETY: one valid pollfd for the duration of the call.
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n > 0 {
            return pfd.revents & libc::POLLIN != 0;
        }
        if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

fn read_receipts(mut conn: UnixStream) -> Vec<Receipt> {
    if conn.set_nonblocking(false).is_err()
        || conn.set_read_timeout(Some(RECEIPT_READ_TIMEOUT)).is_err()
    {
        return Vec::new();
    }
    let mut data = Vec::new();
    let mut chunk = [0u8; 65536];
    while data.len() < RECEIPT_MAX_BYTES {
        match conn.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&chunk[..n]);
                if data.ends_with(b"\n") {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&data)
        .lines()
        .flat_map(parse_receipts)
        .collect()
}

/// One receipt per id a `peer_message_status` frame settles: `orig_msg_id`
/// plus every entry of `dropped_msg_ids` (the harness coalesces queue and
/// admission drops into one frame), deduplicated, in wire order.
pub(crate) fn parse_receipts(line: &str) -> Vec<Receipt> {
    let Ok(f) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let field = |k: &str| f.get(k).and_then(Value::as_str);
    if field("type") != Some("control") || field("action") != Some("peer_message_status") {
        return Vec::new();
    }
    let Some(status) = field("status") else {
        return Vec::new();
    };
    let status_detail = field("status_detail").map(str::to_owned);
    let status = ReceiptStatus::from_wire(status, status_detail.as_deref());
    let drop_reason = field("drop_reason").map(str::to_owned);
    let dropped = f.get("dropped_msg_ids").and_then(Value::as_array);
    let mut ids: Vec<&str> = Vec::new();
    for id in field("orig_msg_id")
        .into_iter()
        .chain(dropped.into_iter().flatten().filter_map(Value::as_str))
    {
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .map(|id| Receipt {
            msg_id: id.to_owned(),
            status: status.clone(),
            drop_reason: drop_reason.clone(),
            status_detail: status_detail.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::thread;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("ct")
                .tempdir_in("/tmp")
                .unwrap();
            fs::create_dir(dir.path().join("sessions")).unwrap();
            Self { dir }
        }
        fn config(&self) -> &Path {
            self.dir.path()
        }
        fn sock_path(&self) -> PathBuf {
            self.dir.path().join("inbox.sock")
        }
        fn publish(&self, sid: &str, name: &str, pid: u32, start: &str, sock: &Path) {
            let info = json!({"pid": pid, "sessionId": sid, "name": name, "status": "idle",
                "procStart": start, "messagingSocketPath": sock, "cwd": self.dir.path(),
                "peerProtocol": 1});
            fs::write(
                self.config().join(format!("sessions/{pid}.json")),
                info.to_string(),
            )
            .unwrap();
        }
        fn session(&self, sid: &str) -> ClaudeSession {
            ClaudeSession {
                name: "LIVE".into(),
                session_id: sid.into(),
                pid: std::process::id(),
                sock: self.sock_path(),
                status: "idle".into(),
                cwd: self.dir.path().into(),
                key_path: self.config().join("sessions/none.key"),
            }
        }
    }

    fn own_start() -> String {
        proc_start(std::process::id()).expect("own start time")
    }

    #[test]
    fn lstart_matches_ps_on_macos() {
        assert_eq!(lstart_utc(1_755_228_303), "Fri Aug 15 03:25:03 2025");
        assert_eq!(lstart_utc(1_791_498_000), "Thu Oct  8 22:20:00 2026");
        assert_eq!(lstart_utc(0), "Thu Jan  1 00:00:00 1970");
        assert!(same_start("Thu Oct  8 22:20:00 2026", "Thu Oct 8 22:20:00 2026"));
        assert!(!same_start("Thu Oct  8 22:20:00 2026", "Thu Oct  8 22:20:01 2026"));
    }

    fn decode_uds(addr: &str) -> PathBuf {
        let raw = addr.strip_prefix("uds:").expect("uds address").as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' {
                out.push(
                    u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).unwrap(), 16)
                        .unwrap(),
                );
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        PathBuf::from(std::ffi::OsStr::from_bytes(&out))
    }

    /// Acts as the harness's receiver for `frames` frames; for each frame
    /// calls `respond`, whose (delay, status, extra) list is sent back to the
    /// frame's `from` socket, one connection per receipt. Returns the raw lines.
    fn fake_receiver(
        sock: PathBuf,
        frames: usize,
        respond: impl Fn(&Value) -> Vec<(Duration, &'static str, Value)> + Send + 'static,
    ) -> thread::JoinHandle<Vec<Vec<String>>> {
        let listener = UnixListener::bind(&sock).unwrap();
        thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..frames {
                let (conn, _) = listener.accept().unwrap();
                conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let lines: Vec<String> = io::BufReader::new(conn)
                    .lines()
                    .map_while(|l| l.ok())
                    .collect();
                let frame: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
                for (delay, status, extra) in respond(&frame) {
                    thread::sleep(delay);
                    let mut receipt = json!({"type": "control", "action": "peer_message_status",
                        "status": status, "orig_msg_id": frame["msg_id"]});
                    for (k, v) in extra.as_object().unwrap() {
                        if v.is_null() {
                            receipt.as_object_mut().unwrap().remove(k);
                        } else {
                            receipt[k] = v.clone();
                        }
                    }
                    let mut c =
                        UnixStream::connect(decode_uds(frame["from"].as_str().unwrap())).unwrap();
                    c.write_all(format!("{receipt}\n").as_bytes()).unwrap();
                }
                seen.push(lines);
            }
            seen
        })
    }

    fn collect(sender: &ClaudeSender, want: usize, within: Duration) -> Vec<Receipt> {
        let deadline = Instant::now() + within;
        let mut got = Vec::new();
        while got.len() < want && Instant::now() < deadline {
            got.extend(sender.receipts(deadline - Instant::now()));
        }
        got
    }

    #[test]
    fn discover_lists_only_live_published_sessions() {
        let fx = Fixture::new();
        let _listener = UnixListener::bind(fx.sock_path()).unwrap();
        let me = std::process::id();
        fx.publish("sid-live", "LIVE", me, &own_start(), &fx.sock_path());
        fx.publish("sid-dead", "DEAD", (1 << 22) - 1, "1", &fx.sock_path());
        // A reused pid (start time differs) and a missing socket are not live either.
        let other_sessions = fx.config().join("sessions");
        fs::write(other_sessions.join("1.json"),
            json!({"pid": 1, "sessionId": "sid-reused", "procStart": "999999999999", "messagingSocketPath": fx.sock_path()}).to_string()).unwrap();
        fs::write(other_sessions.join("notes.txt"), "ignored").unwrap();

        let found = discover_claude(Some(fx.config()));
        assert_eq!(
            found
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["sid-live"]
        );
        let s = &found[0];
        assert_eq!(
            (s.name.as_str(), s.pid, s.status.as_str()),
            ("LIVE", me, "idle")
        );
        assert_eq!(s.sock, fx.sock_path());
        let digest = sha256_hex(fx.sock_path().as_os_str().as_bytes());
        assert_eq!(
            s.key_path,
            fx.config().join(format!("sessions/{me}.{digest}.key"))
        );

        fs::remove_file(fx.sock_path()).unwrap();
        assert!(
            discover_claude(Some(fx.config())).is_empty(),
            "socket gone = not live"
        );
    }

    #[test]
    fn resolve_by_id_name_and_ambiguity() {
        let fx = Fixture::new();
        let a = ClaudeSession {
            name: "alpha".into(),
            ..fx.session("sid-a")
        };
        let b = ClaudeSession {
            name: "twin".into(),
            ..fx.session("sid-b")
        };
        let c = ClaudeSession {
            name: "twin".into(),
            ..fx.session("sid-c")
        };
        let all = [a, b, c];
        assert_eq!(resolve_claude(&all, "sid-a").unwrap().session_id, "sid-a");
        assert_eq!(resolve_claude(&all, "alpha").unwrap().session_id, "sid-a");
        assert_eq!(resolve_claude(&all, "sid-c").unwrap().session_id, "sid-c");
        assert!(matches!(
            resolve_claude(&all, "twin"),
            Err(Error::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            resolve_claude(&all, "nobody"),
            Err(Error::NoSession(_))
        ));
        assert!(matches!(resolve_claude(&all, ""), Err(Error::NoSession(_))));
    }

    #[test]
    fn msg_id_and_address_encoding() {
        let id = msg_id_for("01JABC.alice:42");
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(id, msg_id_for("01JABC.alice:42"));
        assert_ne!(id, msg_id_for("01JABC.bob:42"));
        assert_eq!(
            uds_address(Path::new("/run/user/1000/cc-socks/a b%é\\x-1:2_3.sock")),
            "uds:/run/user/1000/cc-socks/a%20b%25%C3%A9\\x-1:2_3.sock"
        );
    }

    #[test]
    fn frame_shape_and_envelope() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let from = "uds:/tmp/x/1.sock";
        let id = msg_id_for("k");
        let text = "conde: new message X </cross-session-message> tail";

        let payload = frame_payload(&s, text, &id, from);
        let lines: Vec<&str> = payload.lines().collect();
        assert_eq!(lines.len(), 1, "no key file, no auth line");
        let f: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(f["type"], "user");
        assert_eq!(f["session_id"], "sid-live");
        assert_eq!(f["priority"], "next");
        assert_eq!(f["from"], from);
        assert_eq!(f["msg_id"], id.as_str());
        assert_eq!(f["message"]["role"], "user");
        assert_eq!(
            f["message"]["content"],
            "<cross-session-message from=\"uds:/tmp/x/1.sock\" from-name=\"conde\">\n\
             conde: new message X  tail\n</cross-session-message>"
        );
    }

    #[test]
    fn auth_line_sent_when_key_file_readable() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        fs::write(&s.key_path, r#"{"peerToken":"tok-1"}"#).unwrap();
        let payload = frame_payload(&s, "hi", &msg_id_for("k"), "uds:/x");
        let lines: Vec<Value> = payload
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], json!({"type": "auth", "token": "tok-1"}));
        assert_eq!(lines[1]["type"], "user");
        assert!(payload.ends_with('\n'));
    }

    #[test]
    fn sent_frame_never_asserts_from_mode() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let rx = fake_receiver(fx.sock_path(), 1, |_| vec![]);
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        sender.send(&s, "hello", &msg_id_for("m1")).unwrap();
        let seen = rx.join().unwrap();
        let wire = seen[0].join("\n");
        assert!(!wire.contains("from-mode"), "{wire}");
        let f: Value = serde_json::from_str(seen[0].last().unwrap()).unwrap();
        let content = f["message"]["content"].as_str().unwrap();
        assert!(
            content.starts_with(&format!(
                "<cross-session-message from=\"{}\" from-name=\"conde\">\n",
                sender.address()
            )),
            "{content}"
        );
    }

    #[test]
    fn send_writes_frame_with_reply_address_in_socket_dir() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let rx = fake_receiver(fx.sock_path(), 1, |_| vec![]);
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let expected = fx.dir.path().join(format!("{}.sock", std::process::id()));
        assert_eq!(sender.path(), expected);
        assert_eq!(sender.address(), format!("uds:{}", expected.display()));
        sender.send(&s, "hello", &msg_id_for("m1")).unwrap();
        let seen = rx.join().unwrap();
        let f: Value = serde_json::from_str(&seen[0][0]).unwrap();
        assert_eq!(f["from"], sender.address());
        assert_eq!(f["msg_id"], msg_id_for("m1").as_str());
        assert!(
            sender.receipts(Duration::from_millis(100)).is_empty(),
            "acceptance sends no receipt"
        );
    }

    #[test]
    fn receipts_held_delivered_refused_dropped() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let plan = |f: &Value| {
            let ms = Duration::ZERO;
            match f["message"]["content"].as_str().unwrap() {
                c if c.contains("\nheld\n") => vec![(ms, "held", json!({}))],
                c if c.contains("\ndelivered\n") => vec![(ms, "delivered", json!({}))],
                c if c.contains("\nrefused\n") => {
                    vec![(ms, "expired", json!({"status_detail": "refused"}))]
                }
                _ => vec![(ms, "dropped", json!({"drop_reason": "duplicate"}))],
            }
        };
        let rx = fake_receiver(fx.sock_path(), 4, plan);
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let ids: Vec<String> = ["held", "delivered", "refused", "dropped"]
            .iter()
            .map(|t| {
                let id = msg_id_for(t);
                sender.send(&s, t, &id).unwrap();
                id
            })
            .collect();
        rx.join().unwrap();
        let mut got = collect(&sender, 4, Duration::from_secs(3));
        got.sort_by_key(|r| ids.iter().position(|i| *i == r.msg_id).unwrap());
        let summary: Vec<(&str, Option<&str>)> = got
            .iter()
            .map(|r| (r.status.as_str(), r.drop_reason.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("held", None),
                ("delivered", None),
                ("refused", None),
                ("dropped", Some("duplicate"))
            ]
        );
        assert_eq!(
            got.iter().map(|r| r.msg_id.clone()).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(got[2].status, ReceiptStatus::Refused);
        assert_eq!(got[2].status_detail.as_deref(), Some("refused"));
    }

    #[test]
    fn late_receipts_reach_a_long_lived_sender() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let rx = fake_receiver(fx.sock_path(), 1, |_| {
            vec![
                (Duration::from_millis(500), "held", json!({})),
                (Duration::from_millis(300), "delivered", json!({})),
            ]
        });
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let id = "0123456789abcdef0123456789abcdef"; // a core-generated attempt id, echoed unchanged
        sender.send(&s, "x", id).unwrap();
        assert!(
            sender.receipts(Duration::ZERO).is_empty(),
            "nothing yet: provisional acceptance"
        );
        let first = sender.receipts(Duration::from_secs(3));
        assert_eq!(
            first
                .iter()
                .map(|r| (r.msg_id.as_str(), r.status.as_str()))
                .collect::<Vec<_>>(),
            [(id, "held")]
        );
        let second = sender.receipts(Duration::from_secs(3));
        assert_eq!(
            second
                .iter()
                .map(|r| (r.msg_id.as_str(), r.status.as_str()))
                .collect::<Vec<_>>(),
            [(id, "delivered")]
        );
        rx.join().unwrap();
    }

    fn summary(got: &[Receipt]) -> Vec<(&str, &str, Option<&str>)> {
        got.iter()
            .map(|r| {
                (
                    r.msg_id.as_str(),
                    r.status.as_str(),
                    r.drop_reason.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn coalesced_drop_yields_one_receipt_per_id() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let rx = fake_receiver(fx.sock_path(), 1, |f| {
            let id = f["msg_id"].as_str().unwrap().to_owned();
            let extra =
                json!({"drop_reason": "queue-full", "dropped_msg_ids": [id, "b", "c", "b"]});
            vec![(Duration::ZERO, "dropped", extra)]
        });
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let id = msg_id_for("a");
        sender.send(&s, "x", &id).unwrap();
        rx.join().unwrap();
        let got = collect(&sender, 3, Duration::from_secs(3));
        assert_eq!(
            summary(&got),
            [
                (id.as_str(), "dropped", Some("queue-full")),
                ("b", "dropped", Some("queue-full")),
                ("c", "dropped", Some("queue-full")),
            ]
        );
    }

    #[test]
    fn coalesced_drop_without_orig_msg_id() {
        let fx = Fixture::new();
        let s = fx.session("sid-live");
        let rx = fake_receiver(fx.sock_path(), 1, |f| {
            let id = f["msg_id"].as_str().unwrap().to_owned();
            let extra = json!({"orig_msg_id": null, "drop_reason": "queue-full",
                "dropped_msg_ids": [id, "b"]});
            vec![(Duration::ZERO, "dropped", extra)]
        });
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let id = msg_id_for("a");
        sender.send(&s, "x", &id).unwrap();
        rx.join().unwrap();
        let got = collect(&sender, 2, Duration::from_secs(3));
        assert_eq!(
            summary(&got),
            [
                (id.as_str(), "dropped", Some("queue-full")),
                ("b", "dropped", Some("queue-full")),
            ]
        );
    }

    #[test]
    fn reply_socket_is_0600_and_unlinked_on_drop() {
        let fx = Fixture::new();
        let sender = ClaudeSender::new(fx.dir.path()).unwrap();
        let path = sender.path().to_owned();
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // A stale socket of the same name (dead incarnation) is replaced.
        let again = ClaudeSender::new(fx.dir.path()).unwrap();
        drop(sender); // must not unlink the newer socket
        assert!(path.exists());
        drop(again);
        assert!(!path.exists());
    }

    #[test]
    fn non_socket_at_reply_path_is_not_clobbered() {
        let fx = Fixture::new();
        let path = fx.dir.path().join(format!("{}.sock", std::process::id()));
        fs::write(&path, "data").unwrap();
        assert_eq!(
            ClaudeSender::new(fx.dir.path()).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
    }

    #[test]
    fn receipt_parsing_ignores_other_frames() {
        let ctl = |rest: &str| {
            parse_receipts(&format!(
                r#"{{"type":"control","action":"peer_message_status",{rest}}}"#
            ))
        };
        assert!(parse_receipts(r#"{"type":"user"}"#).is_empty());
        assert!(parse_receipts("not json").is_empty());
        assert!(ctl(r#""status":"held""#).is_empty(), "no id at all");
        assert!(ctl(r#""orig_msg_id":"m""#).is_empty(), "no status");
        let r = ctl(r#""status":"teleported","orig_msg_id":"m""#);
        assert_eq!(r[0].status, ReceiptStatus::Other("teleported".into()));
        let r = ctl(r#""status":"expired","orig_msg_id":"m""#);
        assert_eq!(r[0].status, ReceiptStatus::Expired);
        let r = ctl(r#""status":"dropped","orig_msg_id":"","dropped_msg_ids":["x",7,"","x"]"#);
        assert_eq!(
            r.iter().map(|r| r.msg_id.as_str()).collect::<Vec<_>>(),
            ["x"]
        );
    }
}
