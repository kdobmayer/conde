//! `conde update`: replace this binary with a released one, from the same files install.sh
//! reads: `<base>/latest` names the version, `<base>/v<version>/<binary>` and
//! `<base>/v<version>/SHA256SUMS` are the release. The download is checked against its
//! SHA256SUMS line, staged next to the running binary and renamed over it, so the binary is
//! either the old one or the whole new one. Then the bridge service, when one is installed,
//! restarts onto it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

use crate::setup::{self, Env, Host};

/// The dist bucket; the same default as install.sh. CONDE_DIST_BASE_URL overrides it.
pub const DEFAULT_BASE: &str = "https://storage.googleapis.com/conde-cloud-dist";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the downloaded binary may take to print its version before it counts as broken.
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

/// A release version without its `v`; Err unless it has install.sh's shape.
pub fn normalize_version(raw: &str) -> anyhow::Result<String> {
    let v = raw.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let shape = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.+-]+)?$").unwrap();
    if !shape.is_match(v) {
        bail!("not a release version: '{raw}'");
    }
    Ok(v.to_string())
}

/// The release number of a version (`0.2.0-rc.1` -> 0.2.0), for ordering.
pub fn release_number(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let rest = parts.next()?;
    let patch = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
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

pub fn run(opts: Options, out: &mut dyn Write) -> anyhow::Result<i32> {
    let name =
        asset_name(std::env::consts::OS, std::env::consts::ARCH, rosetta()).with_context(|| {
            format!(
                "no conde release is built for {} {}; releases are for Linux x86_64 and macOS",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
    let base = base_url();
    let current = env!("CARGO_PKG_VERSION");
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
        Some(v) => normalize_version(v)?,
        None => {
            let latest = runtime.block_on(fetch(format!("{base}/latest")))?;
            let text = String::from_utf8_lossy(&latest);
            normalize_version(text.lines().next().unwrap_or(""))
                .with_context(|| format!("reading {base}/latest"))?
        }
    };
    if wanted == current {
        writeln!(out, "conde {current} is current")?;
        return Ok(0);
    }
    // only a named --version goes back: a build newer than the latest release stays
    if opts.version.is_none() && release_number(&wanted) < release_number(current) {
        writeln!(
            out,
            "conde {current} is newer than the latest release {wanted}; nothing to do"
        )?;
        return Ok(0);
    }
    if opts.check {
        writeln!(
            out,
            "conde {current} -> {wanted} is available; conde update installs it"
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

    let exe = std::env::current_exe().context("locating the conde binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let staged = Staged::write(&exe, &binary)?;
    let mut host = setup::System;
    clear_quarantine(&mut host, &staged.path);
    let shown = staged.path.to_string_lossy().into_owned();
    match host.run_bounded(&shown, &["--version"], PROBE_LIMIT) {
        Ok(r) if r.ok => {}
        _ => {
            bail!("the downloaded binary does not run on this machine ({name}); nothing installed")
        }
    }
    staged.replace(&exe)?;
    writeln!(out, "conde {current} -> {wanted}")?;

    let env = Env::from_process()?;
    setup::restart_service(&env, &mut host, out)
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

/// The new binary next to the old one, removed unless it replaced it.
struct Staged {
    path: PathBuf,
    done: bool,
}

impl Staged {
    fn write(exe: &Path, bytes: &[u8]) -> anyhow::Result<Staged> {
        let dir = exe.parent().context("the conde binary has no directory")?;
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

    fn replace(mut self, exe: &Path) -> anyhow::Result<()> {
        std::fs::rename(&self.path, exe).with_context(|| format!("replacing {}", exe.display()))?;
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
