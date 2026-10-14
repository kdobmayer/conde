//! The Windows installer keeps its names, paths and integrity contract, and the release
//! workflow publishes it; the release builds no Windows binary (dropped 2026-10-14).

const INSTALL_PS1: &str = include_str!("../../../dist/install.ps1");
const RELEASE: &str = include_str!("../../../.github/workflows/release.yml");
const WINDOWS_BINARY: &str = "conde-windows-x86_64.exe";

#[test]
fn install_ps1_installs_the_released_binary_for_this_user() {
    // Windows PowerShell 5.1 decodes a script served without a charset as ANSI
    assert!(INSTALL_PS1.is_ascii());
    assert!(INSTALL_PS1.contains(&format!("$name = '{WINDOWS_BINARY}'")));
    assert!(INSTALL_PS1.contains(r"Join-Path $env:LOCALAPPDATA 'conde\bin'"));
    assert!(INSTALL_PS1.contains("Join-Path $dir 'conde.exe'"));
    // install.sh's contract: this file's SHA256SUMS line, else nothing installed
    assert!(INSTALL_PS1.contains("-Uri \"$base/SHA256SUMS\""));
    assert!(INSTALL_PS1.contains("Get-FileHash -Algorithm SHA256"));
    assert!(INSTALL_PS1.contains("checksum mismatch for $name; nothing installed"));
    assert!(INSTALL_PS1.contains("SHA256SUMS has no line for $name; nothing installed"));
    // the user PATH, edited with its value kind kept; never the machine's
    assert!(INSTALL_PS1.contains("[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)"));
    assert!(INSTALL_PS1.contains("DoNotExpandEnvironmentNames"));
    assert!(!INSTALL_PS1.contains("'Machine'"));
    // `irm | iex` runs it in the caller's shell: one script block, and no exit
    let code: Vec<&str> =
        INSTALL_PS1.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    assert_eq!((code.first(), code.last()), (Some(&"& {"), Some(&"}")));
    assert!(!code.iter().flat_map(|l| l.split_whitespace()).any(|w| w.eq_ignore_ascii_case("exit")));
}

#[test]
fn the_release_publishes_the_installer_but_builds_no_windows_binary() {
    for needle in ["needs: [web, macos, linux]", "cp dist/install.ps1 out/install.ps1", "-n install.ps1 -f out/install.ps1"] {
        assert!(RELEASE.contains(needle), "release.yml lacks {needle:?}");
    }
    for needle in ["windows-latest", "x86_64-pc-windows-msvc", "conde-windows-"] {
        assert!(!RELEASE.contains(needle), "release.yml still has {needle:?}");
    }
}

/// install.sh against a local release (CONDE_DIST_URL on file://): a first install names the
/// setup and the sign-in; installing again over a conde says what it replaced and names the
/// upgrade's two commands (setup rewrites what changed, doctor shows the bridge's build).
#[cfg(target_os = "linux")]
#[test]
fn install_sh_says_what_it_replaced_and_how_to_finish_an_upgrade() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let (dist, bin) = (root.path().join("dist"), root.path().join("bin"));
    std::fs::create_dir_all(&dist).unwrap();
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    let name = format!("conde-linux-{arch}");
    let release = |version: &str| {
        let fake = format!("#!/bin/sh\necho 'conde 0.1.0 ({version} 2026-10-14)'\n");
        std::fs::write(dist.join(&name), &fake).unwrap();
        std::fs::set_permissions(dist.join(&name), std::fs::Permissions::from_mode(0o755)).unwrap();
        use sha2::Digest;
        let sum: String = sha2::Sha256::digest(fake.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(dist.join("SHA256SUMS"), format!("{sum}  {name}\n")).unwrap();
        std::fs::write(dist.join("latest"), "v0.1.0\n").unwrap();
    };
    // Each run carries its own marker in the environment, inherited by everything it starts, so
    // what a run leaves behind is found without looking at anyone else's processes. Each run has
    // an outer deadline: an installer that hangs fails the test instead of the suite.
    let runs = std::cell::Cell::new(0);
    let install = || {
        runs.set(runs.get() + 1);
        let mark = format!("{}#{}", root.path().display(), runs.get());
        let mut child = std::process::Command::new("sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../dist/install.sh"))
            .env("CONDE_DIST_URL", format!("file://{}", dist.display()))
            .env("CONDE_INSTALL_DIR", &bin)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env(RUN_MARK, &mark)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() > deadline {
                kill(&marked(&mark));
                let _ = child.kill();
                let _ = child.wait();
                panic!("install.sh did not finish within 20 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Checked before reading the output: a leftover holding the pipe would hold that read.
        // A process the installer ended may take a moment to go; a leftover lives for seconds.
        let gone = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut left = marked(&mark);
        while !left.is_empty() && std::time::Instant::now() < gone {
            std::thread::sleep(std::time::Duration::from_millis(20));
            left = marked(&mark);
        }
        if !left.is_empty() {
            kill(&left);
            let _ = child.wait();
            panic!("the installer left processes behind: {left:?}");
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let old = |script: &str, mode: u32| {
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("conde"), script).unwrap();
        std::fs::set_permissions(bin.join("conde"), std::fs::Permissions::from_mode(mode)).unwrap();
    };

    release("aaaaaaa");
    let first = install();
    assert!(first.contains(&format!("installed {}/conde (conde 0.1.0 (aaaaaaa 2026-10-14))", bin.display())), "{first}");
    assert!(first.contains("\nNext:\n  conde setup") && first.contains("conde login --server"), "{first}");
    assert!(!first.contains("replaced") && !first.contains("an upgrade"), "{first}");

    release("bbbbbbb");
    let second = install();
    assert!(
        second.contains(&format!(
            "replaced {}/conde (conde 0.1.0 (aaaaaaa 2026-10-14)) with conde 0.1.0 (bbbbbbb 2026-10-14)",
            bin.display()
        )),
        "{second}"
    );
    assert!(second.contains("Next (an upgrade; your enrollment and names stay as they are, no sign-in):"), "{second}");
    assert!(second.contains("\n  conde setup ") && second.contains("\n  conde doctor "), "{second}");
    assert!(!second.contains("conde login"), "no sign-in on an upgrade: {second}");
    // the old binary answered at once: install() found its watchdog gone with its sleep

    // An old binary that never answers: its probe is cut off after 5 s, the upgrade goes on,
    // and nothing it started is left behind (install() checks; the pid file shows it ran).
    let probe = root.path().join("probe.pid");
    old(&format!("#!/bin/sh\necho $$ >'{}'\nexec sleep 120\n", probe.display()), 0o755);
    let t = std::time::Instant::now();
    let hung = install();
    assert!(std::fs::read_to_string(&probe).unwrap().trim().parse::<i32>().is_ok(), "the probe ran");
    assert!(t.elapsed() < std::time::Duration::from_secs(15), "{:?}", t.elapsed());
    assert!(hung.contains(&format!("replaced {}/conde (unknown version) with conde 0.1.0 (bbbbbbb 2026-10-14)", bin.display())), "{hung}");
    assert!(hung.contains("Next (an upgrade;"), "{hung}");

    // A silent or non-executable old conde is still an upgrade, of an unknown version.
    for (script, mode) in [("#!/bin/sh\nexit 0\n", 0o755), ("not a program\n", 0o644)] {
        old(script, mode);
        let out = install();
        assert!(out.contains(&format!("replaced {}/conde (unknown version) with", bin.display())), "{mode:o}: {out}");
        assert!(out.contains("Next (an upgrade;") && !out.contains("conde login"), "{mode:o}: {out}");
    }
}

#[cfg(target_os = "linux")]
const RUN_MARK: &str = "CONDE_INSTALL_TEST_RUN";

#[cfg(target_os = "linux")]
fn kill(procs: &[(i32, String)]) {
    for (pid, _) in procs {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
}

/// The live processes (pid, command line) whose environment carries this run's marker.
#[cfg(target_os = "linux")]
fn marked(mark: &str) -> Vec<(i32, String)> {
    let needle = format!("{RUN_MARK}={mark}");
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            // unreadable (another user's) or a zombie (an empty environment): not this run's
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|kv| kv == needle.as_bytes()))
        })
        .map(|pid| {
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            (pid, String::from_utf8_lossy(&cmd).replace('\0', " ").trim().to_string())
        })
        .collect()
}
