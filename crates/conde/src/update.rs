//! `conde update`: replace this binary with a released one, from the same files install.sh
//! reads: `<base>/latest` names the version, `<base>/v<version>/<binary>` and
//! `<base>/v<version>/SHA256SUMS` are the release.
//!
//! The target is the file this process runs from, resolved before any download. The download is
//! checked against its SHA256SUMS line, staged next to the target, and must report the wanted
//! version. Then, under a lock file next to the target that serializes installers, the target's
//! version is read again by running it: when another installer replaced it meanwhile, nothing is
//! installed over it. Otherwise the staged file is renamed over the target, so the binary is
//! either the old one or the whole new one, and the bridge service restarts onto it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use semver::Version;
use sha2::{Digest, Sha256};

use crate::setup::{self, Env, Host};

/// The dist bucket; the same default as install.sh. CONDE_DIST_BASE_URL overrides it.
pub const DEFAULT_BASE: &str = "https://storage.googleapis.com/conde-cloud-dist";
/// Next to the binary: held while one `conde update` replaces it.
pub const LOCK_NAME: &str = ".conde-update.lock";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a binary may take to print its version before it counts as broken.
const PROBE_LIMIT: Duration = Duration::from_secs(10);

pub struct Options {
    /// A release to install (`0.2.0` or `v0.2.0`); None: the one `latest` names.
    pub version: Option<String>,
    /// Report only.
    pub check: bool,
}

/// The download base: CONDE_DIST_BASE_URL without a trailing slash, else [`DEFAULT_BASE`].
pub fn base_url() -> String {
    match std::env::var("CONDE_DIST_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    }
}

/// The release file for an OS and CPU as Rust names them (`std::env::consts`); `translated`
/// is an x86_64 process under Rosetta, which gets the native Apple silicon build as install.sh
/// gives it. None: no release is built for it.
pub fn asset_name(os: &str, arch: &str, translated: bool) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("conde-linux-x86_64"),
        ("macos", "aarch64") => Some("conde-darwin-arm64"),
        ("macos", "x86_64") if translated => Some("conde-darwin-arm64"),
        ("macos", "x86_64") => Some("conde-darwin-x86_64"),
        _ => None,
    }
}

/// A release version, with or without its `v`; Err unless it is a SemVer version.
pub fn parse_version(raw: &str) -> anyhow::Result<Version> {
    let v = raw.trim();
    Version::parse(v.strip_prefix('v').unwrap_or(v))
        .with_context(|| format!("not a release version: '{raw}'"))
}

/// The version a conde binary prints for `--version` (`conde 0.2.0 (<commit> <date>)`).
pub fn reported_version(stdout: &str) -> Option<Version> {
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let rest = line.trim().strip_prefix("conde ")?;
    Version::parse(rest.split_whitespace().next()?).ok()
}

/// What `conde update` does about a release.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// The installed binary is that release.
    Current,
    /// The installed binary is newer than the latest release, and no version was named.
    Newer,
    Install,
}

/// By SemVer precedence (a prerelease before its release, build metadata ignored); `named`:
/// the human asked for this version (`--version`), which may go anywhere.
pub fn plan(installed: &str, wanted: &str, named: bool) -> anyhow::Result<Plan> {
    let (installed, wanted) = (parse_version(installed)?, parse_version(wanted)?);
    Ok(match wanted.cmp_precedence(&installed) {
        std::cmp::Ordering::Equal => Plan::Current,
        std::cmp::Ordering::Less if !named => Plan::Newer,
        _ => Plan::Install,
    })
}

/// The lowercase sha256 SHA256SUMS lists for `name` (`<hash>  <name>` or `<hash> *<name>`).
pub fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let line = line.trim_end_matches('\r');
        let (hash, rest) = line.split_at_checked(64)?;
        let file = rest.strip_prefix(' ')?;
        let file = file.strip_prefix([' ', '*'])?;
        (file == name && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// The file to replace: `exe` (this process's `current_exe`) resolved, and still the file this
/// process runs from when `running` (its device and inode) is known. Linux names a replaced
/// executable `<path> (deleted)`; that, a path that no longer resolves, and a path that now names
/// another file are refused, so an update never writes next to or over the wrong file.
pub fn check_target(exe: &Path, running: Option<(u64, u64)>) -> anyhow::Result<PathBuf> {
    if exe.to_string_lossy().ends_with(" (deleted)") {
        bail!(
            "this conde ({}) was replaced while it ran; run conde update again",
            exe.display()
        );
    }
    let target = exe
        .canonicalize()
        .with_context(|| format!("resolving the running binary {}", exe.display()))?;
    #[cfg(unix)]
    if let Some(id) = running {
        use std::os::unix::fs::MetadataExt;
        let m =
            std::fs::metadata(&target).with_context(|| format!("reading {}", target.display()))?;
        if (m.dev(), m.ino()) != id {
            bail!(
                "{} is no longer the binary this conde runs from (another install replaced it); \
                 run conde update again",
                target.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = running;
    Ok(target)
}

/// This process's executable as device and inode; None where the OS does not tell (macOS).
fn running_id() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self/exe")
            .ok()
            .map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// What the binary at `path` says it is: Err when it does not run, None without a version line.
fn probe(host: &mut dyn Host, path: &Path) -> anyhow::Result<Option<Version>> {
    let shown = path.to_string_lossy();
    match host.run_bounded(&shown, &["--version"], PROBE_LIMIT) {
        Ok(r) if r.ok => Ok(reported_version(&r.stdout)),
        Ok(_) => bail!("{} --version failed", path.display()),
        Err(e) => Err(e),
    }
}

/// The installed version: what the target says when run, not what this process was built as.
fn installed_version(host: &mut dyn Host, target: &Path) -> anyhow::Result<Version> {
    probe(host, target)?.with_context(|| format!("{} reports no conde version", target.display()))
}

pub fn run(opts: Options, out: &mut dyn Write) -> anyhow::Result<i32> {
    let name =
        asset_name(std::env::consts::OS, std::env::consts::ARCH, rosetta()).with_context(|| {
            format!(
                "no conde release is built for {} {}; releases are for Linux x86_64 and macOS",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
    let exe = std::env::current_exe().context("locating the conde binary")?;
    let target = check_target(&exe, running_id())?;
    let mut host = setup::System;
    let installed = installed_version(&mut host, &target)?;

    let base = base_url();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(crate::login::tls()?)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    let fetch = |url: String| {
        let client = client.clone();
        async move {
            let response = client
                .get(&url)
                .send()
                .await
                .with_context(|| format!("download failed: {url}"))?;
            if !response.status().is_success() {
                bail!("download failed: {url}: {}", response.status());
            }
            let body = response
                .bytes()
                .await
                .with_context(|| format!("download failed: {url}"))?;
            anyhow::Ok(body.to_vec())
        }
    };

    let wanted = match &opts.version {
        Some(v) => parse_version(v)?,
        None => {
            let latest = runtime.block_on(fetch(format!("{base}/latest")))?;
            let text = String::from_utf8_lossy(&latest);
            parse_version(text.lines().next().unwrap_or(""))
                .with_context(|| format!("reading {base}/latest"))?
        }
    };
    match plan(
        &installed.to_string(),
        &wanted.to_string(),
        opts.version.is_some(),
    )? {
        Plan::Current => {
            writeln!(out, "conde {installed} is current")?;
            return Ok(0);
        }
        Plan::Newer => {
            writeln!(
                out,
                "conde {installed} is newer than the latest release {wanted}; nothing to do"
            )?;
            return Ok(0);
        }
        Plan::Install => {}
    }
    if opts.check {
        writeln!(
            out,
            "conde {installed} -> {wanted} is available; conde update installs it"
        )?;
        return Ok(0);
    }

    let release = format!("{base}/v{wanted}");
    let binary = runtime.block_on(fetch(format!("{release}/{name}")))?;
    let sums = runtime.block_on(fetch(format!("{release}/SHA256SUMS")))?;
    let sums = String::from_utf8_lossy(&sums);
    let Some(listed) = listed_sum(&sums, name) else {
        bail!("SHA256SUMS has no line for {name}; nothing installed");
    };
    let actual: String = Sha256::digest(&binary)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != listed {
        bail!("checksum mismatch for {name}; nothing installed");
    }

    let staged = Staged::write(&target, &binary)?;
    clear_quarantine(&mut host, &staged.path);
    match probe(&mut host, &staged.path) {
        Err(_) => {
            bail!("the downloaded binary does not run on this machine ({name}); nothing installed")
        }
        Ok(None) => bail!("the downloaded {name} reports no conde version; nothing installed"),
        Ok(Some(v)) if v != wanted => {
            bail!("the downloaded {name} reports conde {v}, not {wanted}; nothing installed")
        }
        Ok(Some(_)) => {}
    }

    let _lock = Lock::acquire(&target)?;
    let now = installed_version(&mut host, &target)
        .with_context(|| format!("reading {} again before the replace", target.display()))?;
    if now != installed {
        if now.cmp_precedence(&wanted) != std::cmp::Ordering::Less {
            writeln!(out, "conde {now} is current")?;
            return Ok(0);
        }
        bail!(
            "{} changed to conde {now} while this update ran; nothing installed (run conde update \
             again)",
            target.display()
        );
    }
    staged.replace(&target)?;
    writeln!(out, "conde {installed} -> {wanted}")?;

    let env = Env::from_process()?;
    setup::restart_service(&env, &mut host, out, &target)
        .context("the binary is updated, but restarting the bridge failed")?;
    Ok(0)
}

/// Whether this is an x86_64 process translated by Rosetta (`sysctl.proc_translated` is 1).
fn rosetta() -> bool {
    if !cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        return false;
    }
    std::process::Command::new("sysctl")
        .args(["-n", "sysctl.proc_translated"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

/// curl sets no quarantine flag and neither does reqwest; clear one anyway, as install.sh does.
fn clear_quarantine(host: &mut dyn Host, path: &Path) {
    if cfg!(target_os = "macos") {
        let p = path.to_string_lossy();
        let _ = host.run("xattr", &["-d", "com.apple.quarantine", &p]);
    }
}

/// An exclusive flock on [`LOCK_NAME`] next to the target, released when dropped. The file stays:
/// removing a lock file others may hold open would let two installers lock different files.
struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    fn acquire(target: &Path) -> anyhow::Result<Lock> {
        let path = target.with_file_name(LOCK_NAME);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let flock = |op| loop {
                if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                    return Ok(());
                }
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(e);
                }
            };
            if let Err(e) = flock(libc::LOCK_EX | libc::LOCK_NB) {
                if e.raw_os_error() != Some(libc::EWOULDBLOCK) {
                    return Err(e).with_context(|| format!("locking {}", path.display()));
                }
                eprintln!(
                    "conde update: waiting for another conde update of {} to finish",
                    target.display()
                );
                flock(libc::LOCK_EX).with_context(|| format!("locking {}", path.display()))?;
            }
        }
        Ok(Lock(file))
    }
}

/// The new binary next to the old one, removed unless it replaced it.
struct Staged {
    path: PathBuf,
    done: bool,
}

impl Staged {
    fn write(target: &Path, bytes: &[u8]) -> anyhow::Result<Staged> {
        let dir = target
            .parent()
            .context("the conde binary has no directory")?;
        let path = dir.join(format!(".conde-update.{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("staging the update in {}", dir.display()))?;
        let staged = Staged { path, done: false };
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged.path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(staged)
    }

    fn replace(mut self, target: &Path) -> anyhow::Result<()> {
        std::fs::rename(&self.path, target)
            .with_context(|| format!("replacing {}", target.display()))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
