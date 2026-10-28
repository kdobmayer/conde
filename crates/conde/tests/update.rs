//! `conde update` against a fake dist bucket: it reports, installs a release whose checksum
//! matches, and refuses one that does not, leaving the binary as it was and nothing staged.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod dist_stub;

use std::path::PathBuf;
use std::process::{Command, Output};

use dist_stub::{Bucket, NAME, fake_binary};

const CURRENT: &str = env!("CARGO_PKG_VERSION");

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
            .filter(|n| n != "conde")
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
    for (os, arch) in [
        ("linux", "aarch64"),
        ("windows", "x86_64"),
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
