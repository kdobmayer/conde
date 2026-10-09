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
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
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
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    (!id.is_empty()).then_some(id)
}

/// The OS user that owns this process (the local-mode owner namespace).
pub fn local_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("uid{}", own_uid()))
        .to_lowercase()
}

pub fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The effective uid: what a peer sees through SO_PEERCRED / getpeereid.
pub fn own_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Local-mode trust: a socket peer is accepted only when its uid equals
/// `own`; the error explains the refusal with both values.
pub fn same_user(peer_uid: std::io::Result<u32>, own: u32) -> Result<(), String> {
    match peer_uid {
        Ok(uid) if uid == own => Ok(()),
        Ok(uid) => Err(format!("refused connection from another user: peer uid {uid}, daemon uid {own}")),
        Err(e) => Err(format!("refused connection: peer credentials unreadable ({e}); daemon uid {own}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ids() {
        assert_eq!(host_id("Krisz-MacBook-Pro.local").as_deref(), Some("krisz-macbook-pro.local"));
        assert_eq!(host_id("worker.example.com").as_deref(), Some("worker.example.com"));
        assert_ne!(host_id("worker.a.example"), host_id("worker.b.example"));
        assert_eq!(host_id("homepc\n").as_deref(), Some("homepc"));
        assert_eq!(host_id("Büro_PC.example.com").as_deref(), Some("b-ro-pc.example.com"));
        assert_eq!(host_id("  "), None);
        // the live hostname is readable here and already a valid id
        let live = system_hostname().and_then(|h| host_id(&h)).expect("gethostname");
        assert!(live.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'));
    }

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
    fn ulids_increase() {
        let a = new_ulid();
        let b = new_ulid();
        assert!(a < b);
        assert!(is_ulid(&a));
        assert!(!is_ulid("../etc/passwd"));
        assert!(!is_ulid(&a.to_lowercase()));
    }
}
