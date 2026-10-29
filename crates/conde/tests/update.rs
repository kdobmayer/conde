//! `conde update` against a fake dist bucket: it reports, installs a release whose checksum
//! matches, and refuses one that does not, leaving the binary as it was and nothing staged.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod dist_stub;

use std::path::PathBuf;
use std::process::{Command, Output};

use dist_stub::{Bucket, NAME, fake_binary};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// The lock `conde update` holds next to the binary while it replaces it.
const LOCK: &str = ".conde-update.lock";

/// This build in its own directory (the update replaces the binary it runs as), and a
/// HOME without a bridge service, so the real one is never restarted.
struct Machine {
    root: tempfile::TempDir,
    bin: PathBuf,
}

impl Machine {
    fn new() -> Machine {
        let root = dist_stub::temp_dir();
        let bin = root.path().join("bin/conde");
        dist_stub::place_conde(&bin);
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        Machine { root, bin }
    }

    fn update(&self, bucket: &Bucket, args: &[&str]) -> (Output, String) {
        let out = Command::new(&self.bin)
            .arg("update")
            .args(args)
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("CONDE_DIST_BASE_URL", &bucket.url)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out, text)
    }

    fn binary(&self) -> Vec<u8> {
        std::fs::read(&self.bin).unwrap()
    }

    /// Anything in the binary's directory besides the binary: a staged download left behind.
    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.bin.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "conde" && n != LOCK)
            .collect()
    }
}

fn bucket(m: &Machine) -> Bucket {
    Bucket::serve(&m.root.path().join("bucket"))
}

#[test]
fn check_reports_and_changes_nothing() {
    let m = Machine::new();
    let b = bucket(&m);
    b.release("99.0.0", &fake_binary("99.0.0"), true);
    b.latest("99.0.0");
    let before = m.binary();
    let (out, text) = m.update(&b, &["--check"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains(&format!("conde {CURRENT} -> 99.0.0 is available")),
        "{text}"
    );
    assert_eq!(m.binary(), before);

    b.latest(CURRENT);
    let (out, text) = m.update(&b, &[]);
    assert!(out.status.success(), "{text}");
    assert_eq!(text.trim(), format!("conde {CURRENT} is current"));
    assert_eq!(m.binary(), before);
}

#[test]
fn a_matching_release_replaces_the_binary_and_says_so() {
    let m = Machine::new();
    let b = bucket(&m);
    let new = fake_binary("99.0.0");
    b.release("99.0.0", &new, true);
    b.latest("99.0.0");
    let (out, text) = m.update(&b, &[]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains(&format!("conde {CURRENT} -> 99.0.0\n")),
        "{text}"
    );
    assert!(
        text.contains("service: no bridge service installed"),
        "{text}"
    );
    assert_eq!(m.binary(), new);
    assert!(m.leftovers().is_empty(), "{:?}", m.leftovers());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&m.bin).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn a_build_newer_than_latest_is_kept_unless_a_version_is_named() {
    let m = Machine::new();
    let b = bucket(&m);
    b.release("0.0.1", &fake_binary("0.0.1"), true);
    b.latest("0.0.1");
    let before = m.binary();
    for args in [&[][..], &["--check"][..]] {
        let (out, text) = m.update(&b, args);
        assert!(out.status.success(), "{args:?}: {text}");
        assert!(
            text.contains(&format!(
                "conde {CURRENT} is newer than the latest release 0.0.1"
            )),
            "{args:?}: {text}"
        );
        assert_eq!(m.binary(), before, "{args:?}");
    }
    let (out, text) = m.update(&b, &["--version", "0.0.1"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains(&format!("conde {CURRENT} -> 0.0.1\n")),
        "{text}"
    );
    assert_eq!(m.binary(), fake_binary("0.0.1"));
}

#[test]
fn a_pinned_version_with_or_without_v_is_installed() {
    for pin in ["98.1.0", "v98.1.0"] {
        let m = Machine::new();
        let b = bucket(&m);
        b.release("98.1.0", &fake_binary("98.1.0"), true);
        b.latest("99.0.0");
        let (out, text) = m.update(&b, &["--version", pin]);
        assert!(out.status.success(), "{pin}: {text}");
        assert!(
            text.contains(&format!("conde {CURRENT} -> 98.1.0\n")),
            "{pin}: {text}"
        );
        assert_eq!(m.binary(), fake_binary("98.1.0"));
    }
}

#[test]
fn every_bad_release_fails_and_leaves_the_binary_as_it_was() {
    let m = Machine::new();
    let b = bucket(&m);
    let before = m.binary();
    b.release("97.0.0", &fake_binary("97.0.0"), false);
    b.release("96.0.0", &fake_binary("96.0.0"), true);
    std::fs::write(b.root.join("v96.0.0/SHA256SUMS"), "").unwrap();
    b.release("95.0.0", b"\x7fELF not a program", true);
    let cases: &[(&[&str], &str)] = &[
        (
            &["--version", "97.0.0"],
            &format!("checksum mismatch for {NAME}; nothing installed"),
        ),
        (
            &["--version", "96.0.0"],
            &format!("SHA256SUMS has no line for {NAME}; nothing installed"),
        ),
        (&["--version", "95.0.0"], "does not run on this machine"),
        (
            &["--version", "9.9.9"],
            &format!("download failed: {}/v9.9.9/{NAME}", b.url),
        ),
        (&["--version", "../etc"], "not a release version"),
        (&[], &format!("download failed: {}/latest", b.url)),
    ];
    for (args, want) in cases {
        let (out, text) = m.update(&b, args);
        assert!(!out.status.success(), "{args:?} succeeded: {text}");
        assert!(text.contains(want), "{args:?}: want {want:?} in {text}");
        assert_eq!(m.binary(), before, "{args:?} changed the binary");
        assert!(
            m.leftovers().is_empty(),
            "{args:?} left {:?}",
            m.leftovers()
        );
    }
    std::fs::write(b.root.join("latest"), "<html>oops</html>\n").unwrap();
    let (out, text) = m.update(&b, &[]);
    assert!(
        !out.status.success() && text.contains("not a release version"),
        "{text}"
    );
    assert_eq!(m.binary(), before);
}

#[test]
fn only_released_platforms_have_a_file() {
    use conde::update::asset_name;
    assert_eq!(
        asset_name("linux", "x86_64", false),
        Some("conde-linux-x86_64")
    );
    assert_eq!(
        asset_name("macos", "aarch64", false),
        Some("conde-darwin-arm64")
    );
    assert_eq!(
        asset_name("macos", "x86_64", false),
        Some("conde-darwin-x86_64")
    );
    assert_eq!(
        asset_name("macos", "x86_64", true),
        Some("conde-darwin-arm64")
    );
    assert_eq!(
        asset_name("windows", "x86_64", false),
        Some("conde-windows-x86_64.exe")
    );
    for (os, arch) in [
        ("linux", "aarch64"),
        ("windows", "aarch64"),
        ("freebsd", "x86_64"),
    ] {
        assert_eq!(asset_name(os, arch, false), None, "{os} {arch}");
    }
    let line = format!("{}  {NAME}\r\n", "A".repeat(64));
    assert_eq!(conde::update::listed_sum(&line, NAME), Some("a".repeat(64)));
    assert_eq!(conde::update::listed_sum(&line, "conde-darwin-arm64"), None);
}

/// SemVer precedence: a prerelease is older than its release, and only a named version goes back.
#[test]
fn prereleases_order_by_semver_precedence() {
    use conde::update::{Plan, plan};
    let auto = |installed, wanted| plan(installed, wanted, false).unwrap();
    assert_eq!(
        auto("0.2.0", "0.2.0-rc.1"),
        Plan::Newer,
        "stable -> rc is not an update"
    );
    assert_eq!(
        auto("0.2.0-rc.2", "0.2.0-rc.1"),
        Plan::Newer,
        "rc.2 -> rc.1 is not an update"
    );
    assert_eq!(
        auto("0.2.0-rc.10", "0.2.0-rc.9"),
        Plan::Newer,
        "numeric identifiers compare as numbers"
    );
    assert_eq!(
        auto("0.2.0-rc.1", "0.2.0"),
        Plan::Install,
        "rc -> stable is"
    );
    assert_eq!(auto("0.2.0-rc.1", "0.2.0-rc.2"), Plan::Install);
    assert_eq!(auto("0.9.9", "0.10.0"), Plan::Install);
    assert_eq!(auto("0.2.0", "0.2.0"), Plan::Current);
    for (installed, wanted) in [
        ("0.2.0", "0.2.0-rc.1"),
        ("0.3.0", "0.1.0"),
        ("0.2.0-rc.2", "0.2.0-rc.1"),
    ] {
        assert_eq!(
            plan(installed, wanted, true).unwrap(),
            Plan::Install,
            "a named {wanted} over {installed}"
        );
    }
    for bad in ["0.2", "0.2.0.1", "x.y.z", "0.2.0-"] {
        assert!(plan("0.2.0", bad, true).is_err(), "{bad} must not parse");
    }
}

/// The staged binary must say it is the release it was downloaded as: a checksum-valid file that
/// reports another version, or none, is not installed.
#[test]
fn a_binary_that_is_not_the_wanted_release_is_refused() {
    let m = Machine::new();
    let b = bucket(&m);
    let before = m.binary();
    b.release("99.0.0", &fake_binary("98.0.0"), true);
    b.release("97.0.0", b"#!/bin/sh\nexit 0\n", true);
    let cases = [
        (
            "99.0.0",
            format!("the downloaded {NAME} reports conde 98.0.0, not 99.0.0; nothing installed"),
        ),
        (
            "97.0.0",
            format!("the downloaded {NAME} reports no conde version; nothing installed"),
        ),
    ];
    for (version, want) in cases {
        let (out, text) = m.update(&b, &["--version", version]);
        assert!(!out.status.success(), "{version} succeeded: {text}");
        assert!(text.contains(&want), "{version}: want {want:?} in {text}");
        assert_eq!(m.binary(), before, "{version} changed the binary");
        assert!(
            m.leftovers().is_empty(),
            "{version} left {:?}",
            m.leftovers()
        );
    }
    use conde::update::reported_version;
    let v = |s: &str| semver::Version::parse(s).unwrap();
    assert_eq!(
        reported_version("conde 0.2.0 (abc1234 2026-10-28)\n"),
        Some(v("0.2.0"))
    );
    assert_eq!(
        reported_version("conde 0.3.0-rc.1 (unknown)"),
        Some(v("0.3.0-rc.1"))
    );
    for out in ["", "\n", "0.2.0", "kond 0.2.0 (x)", "conde nope (x)"] {
        assert_eq!(reported_version(out), None, "{out:?}");
    }
}

impl Machine {
    /// `conde update` started in the background, its stderr lines on a channel.
    fn spawn(&self, bucket: &Bucket) -> (std::process::Child, std::sync::mpsc::Receiver<String>) {
        use std::io::BufRead;
        let mut child = Command::new(&self.bin)
            .arg("update")
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("CONDE_DIST_BASE_URL", &bucket.url)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let _ = tx.send(line);
            }
        });
        (child, rx)
    }

    /// What another installer does: a whole new binary renamed over this one.
    fn installed_by_another(&self, version: &str) {
        let tmp = self.bin.with_file_name(".other-installer");
        std::fs::write(&tmp, fake_binary(version)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &self.bin).unwrap();
    }

    fn no_deleted_sibling(&self) {
        let names: Vec<String> = std::fs::read_dir(self.bin.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.contains("(deleted)")), "{names:?}");
    }
}

fn finish(child: std::process::Child) -> (bool, String) {
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A downloaded and checked 99 waits for the lock while another installer puts 100 in place:
/// under the lock the update reads the binary again, finds the newer one and installs nothing.
#[test]
fn an_update_waiting_for_the_lock_never_installs_over_a_newer_binary() {
    let m = Machine::new();
    let b = bucket(&m);
    b.release("99.0.0", &fake_binary("99.0.0"), true);
    b.latest("99.0.0");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(m.bin.with_file_name(LOCK))
        .unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (child, stderr) = m.spawn(&b);
    let waiting = stderr
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("no line from the update ({e}); it did not wait for the lock"));
    assert!(
        waiting.contains("waiting for another conde update"),
        "{waiting}"
    );
    m.installed_by_another("100.0.0");
    drop(lock);
    let (ok, text) = finish(child);
    assert!(ok, "{text}");
    assert!(text.contains("conde 100.0.0 is current"), "{text}");
    assert_eq!(m.binary(), fake_binary("100.0.0"));
    m.no_deleted_sibling();
    assert!(m.leftovers().is_empty(), "{:?}", m.leftovers());
}

/// Another installer replaces the binary while the download is under way: the update never
/// installs over the newer one, refuses over an older one, and never writes a "(deleted)" file.
#[test]
fn a_binary_replaced_during_the_download_is_read_again_before_the_replace() {
    for (other, ok_wanted, says) in [
        ("100.0.0", true, "conde 100.0.0 is current".to_string()),
        (
            "98.0.0",
            false,
            "changed to conde 98.0.0 while this update ran; nothing installed".to_string(),
        ),
    ] {
        let m = Machine::new();
        let b = bucket(&m);
        b.release("99.0.0", &fake_binary("99.0.0"), true);
        b.latest("99.0.0");
        let gate = b.hold(&format!("v99.0.0/{NAME}"));
        let (child, stderr) = m.spawn(&b);
        gate.wait_arrived(std::time::Duration::from_secs(60));
        m.installed_by_another(other);
        gate.open();
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            // every line: the reader ends at the exited child's end of stream
            stderr.iter().collect::<Vec<_>>().join("\n")
        );
        assert_eq!(out.status.success(), ok_wanted, "{other}: {text}");
        assert!(text.contains(&says), "{other}: want {says:?} in {text}");
        assert_eq!(
            m.binary(),
            fake_binary(other),
            "{other}: the other installer's binary stays"
        );
        m.no_deleted_sibling();
        assert!(m.leftovers().is_empty(), "{other}: {:?}", m.leftovers());
    }
}

/// The target is the file this process runs from, resolved before any download: a path the
/// kernel marks deleted, one that no longer exists, or one that names another file is refused.
#[test]
fn the_target_must_be_the_running_binary() {
    use conde::update::check_target;
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let id = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.dev(), m.ino())
    };
    assert_eq!(
        check_target(&a, Some(id(&a))).unwrap(),
        a.canonicalize().unwrap()
    );
    let err = |r: anyhow::Result<PathBuf>| format!("{:#}", r.unwrap_err());
    let deleted = PathBuf::from(format!("{} (deleted)", a.display()));
    assert!(err(check_target(&deleted, Some(id(&a)))).contains("was replaced while it ran"));
    assert!(err(check_target(&dir.path().join("gone"), None)).contains("resolving"));
    assert!(
        err(check_target(&a, Some(id(&b)))).contains("no longer the binary this conde runs from")
    );
}

/// Two real installers of one binary, both mid-download, then released together: the lock lets
/// one install and the other find that release already in place.
#[test]
fn two_concurrent_updates_install_once() {
    let m = Machine::new();
    let b = bucket(&m);
    b.release("99.0.0", &fake_binary("99.0.0"), true);
    b.latest("99.0.0");
    let gate = b.hold(&format!("v99.0.0/{NAME}"));
    let (first, _) = m.spawn(&b);
    let (second, _) = m.spawn(&b);
    gate.wait_requests(2, std::time::Duration::from_secs(60));
    gate.open();
    let outs = [finish(first), finish(second)];
    let installed = outs
        .iter()
        .filter(|(ok, t)| *ok && t.contains(&format!("conde {CURRENT} -> 99.0.0")))
        .count();
    let current = outs
        .iter()
        .filter(|(ok, t)| *ok && t.contains("conde 99.0.0 is current"))
        .count();
    assert_eq!((installed, current), (1, 1), "{outs:?}");
    assert_eq!(m.binary(), fake_binary("99.0.0"));
    m.no_deleted_sibling();
    assert!(m.leftovers().is_empty(), "{:?}", m.leftovers());
}
