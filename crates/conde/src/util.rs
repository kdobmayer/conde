//! Small helpers: ids, time, host and user facts, digests.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use ulid::{Generator, Ulid};

static ULIDS: Mutex<Generator> = Mutex::new(Generator::new());

/// A new ULID, strictly increasing within this process.
pub fn new_ulid() -> String {
    let mut generator = ULIDS.lock().unwrap_or_else(|e| e.into_inner());
    let ulid = match generator.generate() {
        Ok(u) => u,
        Err(overflow) => overflow.commit_overflow_random(),
    };
    ulid.to_string()
}

/// Whether `s` is a canonical (uppercase Crockford) ULID.
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26 && Ulid::from_string(s).is_ok_and(|u| u.to_string() == s)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the Date header format kond used.
pub fn iso_from_ms(ms: i64) -> String {
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Milliseconds since the epoch of an RFC 3339 date-time (`2026-10-09T14:30:00Z`,
/// `2026-10-09T16:30:00.5+02:00`), of conde's own `YYYY-MM-DDTHH:MM:SS.mmmZ`, or of the
/// minute its listings print (`2026-10-09T14:30`); a time without a zone is UTC, as conde
/// prints every date. Fractions finer than a millisecond are dropped.
pub fn ms_from_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(at(4, b'-')
        && at(7, b'-')
        && matches!(b.get(10), Some(b'T' | b't' | b' '))
        && at(13, b':'))
    {
        return None;
    }
    let (y, mo, d, h, mi) = (num(0, 4)?, num(5, 2)?, num(8, 2)?, num(11, 2)?, num(14, 2)?);
    let mut i = 16;
    let (mut sec, mut milli) = (0, 0);
    if at(i, b':') {
        sec = num(i + 1, 2)?;
        i += 3;
        if at(i, b'.') {
            let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 {
                return None;
            }
            milli = (0..3)
                .map(|k| {
                    if k < digits {
                        i64::from(b[i + 1 + k] - b'0')
                    } else {
                        0
                    }
                })
                .fold(0, |n, d| n * 10 + d);
            i += 1 + digits;
        }
    }
    let offset_min = match b.get(i..) {
        Some([] | [b'Z' | b'z']) => 0,
        Some([sign @ (b'+' | b'-'), _, _, b':', _, _]) => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            (oh * 60 + om) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&mo)
        || d < 1
        || d > month_days[(mo - 1) as usize]
        || h > 23
        || mi > 59
        || sec > 60
    {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60) * 1000) + milli)
}

// Howard Hinnant's civil date -> days algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Where a listing starts: after a cursor (`seq`), keeping only messages dated after a time
/// when one is given. A time search continues with both, as the token `<seq>@<time>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct After {
    pub cursor: i64,
    pub time: Option<i64>,
}

impl After {
    pub fn cursor(cursor: i64) -> After {
        After { cursor, time: None }
    }

    /// The request field and the printed continuation: a plain cursor as a number (as every
    /// daemon takes it), a time alone as conde's date, both as `<seq>@<date>`.
    pub fn wire(self) -> serde_json::Value {
        match self.time {
            None => self.cursor.into(),
            Some(t) if self.cursor == 0 => iso_from_ms(t).into(),
            Some(t) => format!("{}@{}", self.cursor, iso_from_ms(t)).into(),
        }
    }
}

/// The accepted `after` forms, for help texts and refusals.
pub const AFTER_FORMS: &str = "a cursor (what a previous page printed after --after) or a time: RFC 3339 \
(2026-10-09T14:30:00Z, 2026-10-09T16:30:00+02:00), conde's own dates (2026-10-09T14:30:00.000Z), \
or the minute listings print (2026-10-09T14:30); a time without a zone is UTC";

/// The one `after` rule of the CLI, the MCP tool and the daemon.
pub fn parse_after(s: &str) -> Result<After, String> {
    let s = s.trim();
    let bad = || format!("'{s}' is not {AFTER_FORMS}");
    if let Ok(n) = s.parse::<i64>() {
        return Ok(After::cursor(n));
    }
    if let Some((n, t)) = s.split_once('@') {
        let cursor = n.parse::<i64>().map_err(|_| bad())?;
        return ms_from_iso(t)
            .map(|t| After {
                cursor,
                time: Some(t),
            })
            .ok_or_else(bad);
    }
    ms_from_iso(s)
        .map(|t| After {
            cursor: 0,
            time: Some(t),
        })
        .ok_or_else(bad)
}

// Howard Hinnant's days -> civil date algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m as u32, d as u32)
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Host id used when no hostname can be read; doctor warns about it.
pub const FALLBACK_HOST: &str = "localhost";

/// This machine's host id: `$CONDE_HOST`, else the system hostname
/// (gethostname, every Unix), else `$HOSTNAME`, else `localhost`.
pub fn local_host() -> String {
    if let Some(h) = std::env::var("CONDE_HOST").ok().filter(|h| !h.is_empty()) {
        return h.to_lowercase();
    }
    system_hostname()
        .and_then(|h| host_id(&h))
        .or_else(|| std::env::var("HOSTNAME").ok().and_then(|h| host_id(&h)))
        .unwrap_or_else(|| FALLBACK_HOST.into())
}

#[cfg(windows)]
fn system_hostname() -> Option<String> {
    crate::win::hostname()
}

#[cfg(unix)]
fn system_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates
    // on success except on truncation, which the length cap below handles.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// A raw hostname as a host id: the full name, lowercased, with characters
/// outside `[a-z0-9.-]` turned into `-`. Never shortened: distinct hostnames
/// must stay distinct, and a board's existing bindings keep their host.
pub fn host_id(raw: &str) -> Option<String> {
    let id: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    (!id.is_empty()).then_some(id)
}

/// The variable naming the login user: `USER`, on Windows `USERNAME`.
pub const USER_VAR: &str = if cfg!(windows) { "USERNAME" } else { "USER" };

/// The OS user that owns this process (the local-mode owner namespace).
pub fn local_user() -> String {
    std::env::var(USER_VAR)
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(unnamed_user)
        .to_lowercase()
}

#[cfg(unix)]
fn unnamed_user() -> String {
    format!("uid{}", own_uid())
}

#[cfg(windows)]
fn unnamed_user() -> String {
    "user".into()
}

#[cfg(unix)]
pub fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The effective uid: what a peer sees through SO_PEERCRED / getpeereid.
#[cfg(unix)]
pub fn own_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Local-mode trust: a socket peer is accepted only when its uid equals
/// `own`; the error explains the refusal with both values.
pub fn same_user(peer_uid: std::io::Result<u32>, own: u32) -> Result<(), String> {
    match peer_uid {
        Ok(uid) if uid == own => Ok(()),
        Ok(uid) => Err(format!(
            "refused connection from another user: peer uid {uid}, daemon uid {own}"
        )),
        Err(e) => Err(format!(
            "refused connection: peer credentials unreadable ({e}); daemon uid {own}"
        )),
    }
}

/// Windows local-mode trust: a pipe peer is accepted only when its process runs as
/// the user (SID) `own`.
pub fn same_sid(peer_sid: std::io::Result<String>, own: &str) -> Result<(), String> {
    match peer_sid {
        Ok(sid) if sid == own => Ok(()),
        Ok(sid) => Err(format!(
            "refused connection from another user: peer SID {sid}, daemon SID {own}"
        )),
        Err(e) => Err(format!(
            "refused connection: peer process unreadable ({e}); daemon SID {own}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ids() {
        assert_eq!(
            host_id("Krisz-MacBook-Pro.local").as_deref(),
            Some("krisz-macbook-pro.local")
        );
        assert_eq!(
            host_id("worker.example.com").as_deref(),
            Some("worker.example.com")
        );
        assert_ne!(host_id("worker.a.example"), host_id("worker.b.example"));
        assert_eq!(host_id("homepc\n").as_deref(), Some("homepc"));
        assert_eq!(
            host_id("Büro_PC.example.com").as_deref(),
            Some("b-ro-pc.example.com")
        );
        assert_eq!(host_id("  "), None);
        // the live hostname is readable here and already a valid id
        let live = system_hostname()
            .and_then(|h| host_id(&h))
            .expect("gethostname");
        assert!(
            live.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        );
    }

    #[cfg(unix)]
    #[test]
    fn peers_are_accepted_only_with_the_same_uid() {
        assert_eq!(same_user(Ok(501), 501), Ok(()));
        let err = same_user(Ok(502), 501).unwrap_err();
        assert!(err.contains("peer uid 502, daemon uid 501"), "{err}");
        let err = same_user(Err(std::io::Error::other("gone")), 501).unwrap_err();
        assert!(err.contains("unreadable (gone); daemon uid 501"), "{err}");
        // this process's own uid is the one its peers report
        assert_eq!(same_user(Ok(own_euid()), own_euid()), Ok(()));
    }

    #[test]
    fn pipe_peers_are_accepted_only_with_the_same_sid() {
        let own = "S-1-5-21-1-2-3-1001";
        assert_eq!(same_sid(Ok(own.into()), own), Ok(()));
        let err = same_sid(Ok("S-1-5-21-1-2-3-1002".into()), own).unwrap_err();
        assert!(
            err.contains("peer SID S-1-5-21-1-2-3-1002, daemon SID S-1-5-21-1-2-3-1001"),
            "{err}"
        );
        let err = same_sid(Err(std::io::Error::other("gone")), own).unwrap_err();
        assert!(err.contains("unreadable (gone)"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn own_uid_is_the_real_uid() {
        use std::os::unix::fs::MetadataExt;
        let probe = std::env::temp_dir().join(format!("conde-uid-{}", new_ulid()));
        std::fs::write(&probe, b"").unwrap();
        let owner = std::fs::metadata(&probe).unwrap().uid();
        std::fs::remove_file(&probe).unwrap();
        assert_eq!(own_uid(), owner);
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_755_228_303_063), "2025-08-15T03:25:03.063Z");
        assert_eq!(iso_from_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn iso_dates_parse_back() {
        for ms in [
            0,
            1_755_228_303_063,
            951_782_400_000,
            -86_400_000,
            4_102_444_799_999,
        ] {
            assert_eq!(ms_from_iso(&iso_from_ms(ms)), Some(ms), "{ms}");
        }
        let t = ms_from_iso("2026-10-09T14:30:00Z").unwrap();
        assert_eq!(ms_from_iso("2026-10-09T16:30:00+02:00"), Some(t));
        assert_eq!(
            ms_from_iso("2026-10-09T12:30:00-02:30"),
            Some(t + 30 * 60_000)
        );
        assert_eq!(ms_from_iso("2026-10-09t14:30:00z"), Some(t));
        assert_eq!(ms_from_iso("2026-10-09 14:30:00Z"), Some(t));
        assert_eq!(
            ms_from_iso("2026-10-09T14:30"),
            Some(t),
            "the minute listings print"
        );
        assert_eq!(ms_from_iso("2026-10-09T14:30:00"), Some(t));
        assert_eq!(ms_from_iso("2026-10-09T14:30:00.5Z"), Some(t + 500));
        assert_eq!(ms_from_iso("2026-10-09T14:30:00.123456789Z"), Some(t + 123));
        for bad in [
            "",
            "yesterday",
            "2026-10-09",
            "2026-13-01T00:00Z",
            "2026-02-29T00:00Z",
            "2026-10-09T24:00Z",
            "2026-10-09T14:30:00.Z",
            "2026-10-09T14:30:00+0200",
            "2026-10-09T14:30:00 Z",
            "2026-1-09T14:30Z",
            "2026-10-09T14:30:00Z junk",
        ] {
            assert_eq!(ms_from_iso(bad), None, "{bad}");
        }
        assert_eq!(ms_from_iso("2024-02-29T00:00Z"), Some(1_709_164_800_000));
    }

    #[test]
    fn after_is_a_cursor_or_a_time() {
        assert_eq!(parse_after("42"), Ok(After::cursor(42)));
        assert_eq!(parse_after(" 0 "), Ok(After::cursor(0)));
        let time = After {
            cursor: 0,
            time: Some(1000),
        };
        assert_eq!(parse_after("1970-01-01T00:00:01Z"), Ok(time));
        assert_eq!(time.wire(), "1970-01-01T00:00:01.000Z");
        let token = After {
            cursor: 205,
            time: Some(1000),
        };
        assert_eq!(token.wire(), "205@1970-01-01T00:00:01.000Z");
        assert_eq!(parse_after("205@1970-01-01T00:00:01.000Z"), Ok(token));
        assert_eq!(After::cursor(7).wire(), 7);
        for bad in ["x@1970-01-01T00:00:01Z", "205@yesterday", "205@"] {
            assert!(parse_after(bad).is_err(), "{bad}");
        }
        let e = parse_after("yesterday").unwrap_err();
        assert!(
            e.starts_with("'yesterday' is not a cursor") && e.contains("RFC 3339"),
            "{e}"
        );
    }

    #[test]
    fn ulids_increase() {
        let a = new_ulid();
        let b = new_ulid();
        assert!(a < b);
        assert!(is_ulid(&a));
        assert!(!is_ulid("../etc/passwd"));
        assert!(!is_ulid(&a.to_lowercase()));
    }
}
