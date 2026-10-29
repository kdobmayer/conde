//! The Windows installer keeps its names, paths and integrity contract, reads the bucket layout
//! install.sh reads, and the release workflow builds its binary as CI does and publishes both.

const INSTALL_PS1: &str = include_str!("../../../dist/install.ps1");
const INSTALL_SH: &str = include_str!("../../../dist/install.sh");
const RELEASE: &str = include_str!("../../../.github/workflows/release.yml");
const CI: &str = include_str!("../../../.github/workflows/ci.yml");
const WINDOWS_BINARY: &str = "conde-windows-x86_64.exe";

#[test]
fn install_ps1_installs_the_released_binary_for_this_user() {
    // Windows PowerShell 5.1 decodes a script served without a charset as ANSI
    assert!(INSTALL_PS1.is_ascii());
    assert!(INSTALL_PS1.contains(&format!("$name = '{WINDOWS_BINARY}'")));
    assert!(INSTALL_PS1.contains(r"Join-Path $env:LOCALAPPDATA 'conde\bin'"));
    assert!(INSTALL_PS1.contains("Join-Path $dir 'conde.exe'"));
    // the bucket layout install.sh and `conde update` read, from the same default base
    assert!(INSTALL_PS1.contains(&format!(
        "if ($env:CONDE_DIST_BASE_URL) {{ $env:CONDE_DIST_BASE_URL.TrimEnd('/') }} else {{ '{}' }}",
        conde::update::DEFAULT_BASE
    )));
    assert!(INSTALL_PS1.contains("\"$base/latest\""));
    assert!(INSTALL_PS1.contains("$url = \"$base/v$version\""));
    // install.sh's contract: this file's SHA256SUMS line, else nothing installed
    assert!(INSTALL_PS1.contains("-Uri \"$url/$name\""));
    assert!(INSTALL_PS1.contains("-Uri \"$url/SHA256SUMS\""));
    assert!(INSTALL_PS1.contains("Get-FileHash -Algorithm SHA256"));
    assert!(INSTALL_PS1.contains("checksum mismatch for $name; nothing installed"));
    assert!(INSTALL_PS1.contains("SHA256SUMS has no line for $name; nothing installed"));
    // the user PATH, edited with its value kind kept; never the machine's
    assert!(
        INSTALL_PS1
            .contains("[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)")
    );
    assert!(INSTALL_PS1.contains("DoNotExpandEnvironmentNames"));
    assert!(!INSTALL_PS1.contains("'Machine'"));
    // `irm | iex` runs it in the caller's shell: one script block, and no exit
    let code: Vec<&str> = INSTALL_PS1
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!((code.first(), code.last()), (Some(&"& {"), Some(&"}")));
    assert!(
        !code
            .iter()
            .flat_map(|l| l.split_whitespace())
            .any(|w| w.eq_ignore_ascii_case("exit"))
    );
}

/// What install.sh tells a new user and an upgrading one to run next, install.ps1 says in the
/// same words.
#[test]
fn install_ps1_prints_install_sh_next_lines() {
    let said: Vec<&str> = INSTALL_SH
        .lines()
        .filter_map(|l| l.trim().strip_prefix("say \"")?.strip_suffix('"'))
        .filter(|l| {
            l.starts_with("Next") || l.starts_with("  conde ") || l.starts_with("(No workspace")
        })
        .collect();
    assert_eq!(said.len(), 6, "install.sh's Next: lines: {said:?}");
    for line in said {
        assert!(
            INSTALL_PS1.contains(&format!("Write-Host \"{line}\"")),
            "install.ps1 does not print {line:?}"
        );
    }
}

/// The steps from `- name: Toolchain` through the Build step's run line of one job, its target
/// and flags; a build cache changes no output and is left out.
fn build_steps<'a>(workflow: &'a str, job: &str) -> Vec<&'a str> {
    let body: Vec<&str> = workflow
        .lines()
        .skip_while(|l| *l != format!("  {job}:"))
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with("    "))
        .collect();
    let env: Vec<&str> = body
        .iter()
        .copied()
        .filter(|l| {
            l.trim_start().starts_with("TARGET:") || l.trim_start().starts_with("RUSTFLAGS:")
        })
        .collect();
    let steps = body
        .iter()
        .copied()
        .skip_while(|l| l.trim() != "- name: Toolchain")
        .take_while(|l| !l.trim_start().starts_with("run: cargo build"))
        .filter(|l| !l.contains("Swatinem/rust-cache"));
    let run = body
        .iter()
        .copied()
        .find(|l| l.trim_start().starts_with("run: cargo build"));
    env.into_iter().chain(steps).chain(run).collect()
}

#[test]
fn the_release_builds_and_publishes_the_windows_binary_as_ci_tests_it() {
    for needle in [
        "needs: [build-id, web, macos, linux, windows]",
        "conde-linux-x86_64 conde-windows-x86_64.exe\"",
        "dist/install.ps1 \"gs://$DIST_BUCKET/install.ps1\"",
    ] {
        assert!(RELEASE.contains(needle), "release.yml lacks {needle:?}");
    }
    let release = build_steps(RELEASE, "windows");
    assert!(
        release.len() > 5,
        "release.yml's windows build: {release:?}"
    );
    assert!(release.iter().any(|l| l.contains("+crt-static")));
    assert_eq!(build_steps(CI, "windows-install"), release);
}

/// install.sh against a local release (CONDE_DIST_BASE_URL on file://, the bucket's v<version>/
/// layout): a first install names the sign-in, which sets the machine up; installing again over a conde says
/// what it replaced and names the upgrade's two commands (setup rewrites what changed, doctor
/// shows the bridge's build).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_sh_says_what_it_replaced_and_how_to_finish_an_upgrade() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let (dist, bin) = (root.path().join("dist"), root.path().join("bin"));
    let release_dir = dist.join("v0.1.0");
    std::fs::create_dir_all(&release_dir).unwrap();
    let name = "conde-linux-x86_64";
    let release = |version: &str| {
        let fake = format!("#!/bin/sh\necho 'conde 0.1.0 ({version} 2026-10-14)'\n");
        std::fs::write(release_dir.join(name), &fake).unwrap();
        std::fs::set_permissions(
            release_dir.join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        use sha2::Digest;
        let sum: String = sha2::Sha256::digest(fake.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(release_dir.join("SHA256SUMS"), format!("{sum}  {name}\n")).unwrap();
        std::fs::write(dist.join("latest"), "v0.1.0\n").unwrap();
    };
    // Each run carries its own marker in the environment, inherited by everything it starts, so
    // what a run leaves behind is found without looking at anyone else's processes. Each run has
    // an outer deadline: an installer that hangs fails the test instead of the suite.
    let runs = std::cell::Cell::new(0);
    let install = || {
        runs.set(runs.get() + 1);
        let mark = format!("{}#{}", root.path().display(), runs.get());
        let mut child = std::process::Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../dist/install.sh"
            ))
            .env("CONDE_DIST_BASE_URL", format!("file://{}", dist.display()))
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
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let old = |script: &str, mode: u32| {
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("conde"), script).unwrap();
        std::fs::set_permissions(bin.join("conde"), std::fs::Permissions::from_mode(mode)).unwrap();
    };

    release("aaaaaaa");
    let first = install();
    assert!(
        first.contains(&format!(
            "installed {}/conde (conde 0.1.0 (aaaaaaa 2026-10-14))",
            bin.display()
        )),
        "{first}"
    );
    assert!(
        first.contains("\nNext:\n  conde login --server https://<workspace>.conde.cloud ")
            && !first.contains("\n  conde setup"),
        "{first}"
    );
    assert!(
        !first.contains("replaced") && !first.contains("an upgrade"),
        "{first}"
    );

    release("bbbbbbb");
    let second = install();
    assert!(
        second.contains(&format!(
            "replaced {}/conde (conde 0.1.0 (aaaaaaa 2026-10-14)) with conde 0.1.0 (bbbbbbb 2026-10-14)",
            bin.display()
        )),
        "{second}"
    );
    assert!(
        second
            .contains("Next (an upgrade; your enrollment and names stay as they are, no sign-in):"),
        "{second}"
    );
    assert!(
        second.contains("\n  conde setup ") && second.contains("\n  conde doctor "),
        "{second}"
    );
    assert!(
        !second.contains("conde login"),
        "no sign-in on an upgrade: {second}"
    );
    // the old binary answered at once: install() found its watchdog gone with its sleep

    // An old binary that never answers: its probe is cut off after 5 s, the upgrade goes on,
    // and nothing it started is left behind (install() checks; the pid file shows it ran).
    let probe = root.path().join("probe.pid");
    old(
        &format!(
            "#!/bin/sh\necho $$ >'{}'\nexec sleep 120\n",
            probe.display()
        ),
        0o755,
    );
    let t = std::time::Instant::now();
    let hung = install();
    assert!(
        std::fs::read_to_string(&probe)
            .unwrap()
            .trim()
            .parse::<i32>()
            .is_ok(),
        "the probe ran"
    );
    assert!(
        t.elapsed() < std::time::Duration::from_secs(15),
        "{:?}",
        t.elapsed()
    );
    assert!(
        hung.contains(&format!(
            "replaced {}/conde (unknown version) with conde 0.1.0 (bbbbbbb 2026-10-14)",
            bin.display()
        )),
        "{hung}"
    );
    assert!(hung.contains("Next (an upgrade;"), "{hung}");

    // A silent or non-executable old conde is still an upgrade, of an unknown version.
    for (script, mode) in [("#!/bin/sh\nexit 0\n", 0o755), ("not a program\n", 0o644)] {
        old(script, mode);
        let out = install();
        assert!(
            out.contains(&format!(
                "replaced {}/conde (unknown version) with",
                bin.display()
            )),
            "{mode:o}: {out}"
        );
        assert!(
            out.contains("Next (an upgrade;") && !out.contains("conde login"),
            "{mode:o}: {out}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUN_MARK: &str = "CONDE_INSTALL_TEST_RUN";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn kill(procs: &[(i32, String)]) {
    for (pid, _) in procs {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
}

/// The live processes (pid, command line) whose environment carries this run's marker.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn marked(mark: &str) -> Vec<(i32, String)> {
    let needle = format!("{RUN_MARK}={mark}");
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            // unreadable (another user's) or a zombie (an empty environment): not this run's
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|kv| kv == needle.as_bytes()))
        })
        .map(|pid| {
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            (
                pid,
                String::from_utf8_lossy(&cmd)
                    .replace('\0', " ")
                    .trim()
                    .to_string(),
            )
        })
        .collect()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod dist_stub;

/// install.sh and `conde update` read one layout: the same default base, and from one fake
/// bucket both install the same release bytes and refuse the same bad checksum in the same words.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_sh_and_conde_update_agree_on_the_layout() {
    use dist_stub::{Bucket, NAME, fake_binary};
    let script = include_str!("../../../dist/install.sh");
    let default = script
        .lines()
        .find_map(|l| l.strip_prefix("base=${CONDE_DIST_BASE_URL:-"))
        .and_then(|rest| rest.strip_suffix('}'))
        .expect("install.sh sets base from CONDE_DIST_BASE_URL");
    assert_eq!(default, conde::update::DEFAULT_BASE);

    let root = dist_stub::temp_dir();
    let bucket = Bucket::serve(&root.path().join("bucket"));
    let release = fake_binary("99.0.0");
    bucket.release("99.0.0", &release, true);
    bucket.release("97.0.0", &fake_binary("97.0.0"), false);
    bucket.latest("99.0.0");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let run = |cmd: &mut std::process::Command, vars: &[(&str, &std::ffi::OsStr)]| {
        let out = cmd
            .env_clear()
            .envs(vars.iter().copied())
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("CONDE_DIST_BASE_URL", &bucket.url)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    };

    let by_script = root.path().join("script");
    let install = |version: Option<&str>| {
        let mut cmd = std::process::Command::new("bash");
        cmd.arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../dist/install.sh"
        ));
        let mut vars = vec![("CONDE_INSTALL_DIR", by_script.as_os_str())];
        if let Some(v) = version {
            vars.push(("CONDE_VERSION", std::ffi::OsStr::new(v)));
        }
        run(&mut cmd, &vars)
    };
    let by_update = root.path().join("update/conde");
    dist_stub::place_conde(&by_update);
    let update = |args: &[&str]| {
        run(
            std::process::Command::new(&by_update)
                .arg("update")
                .args(args),
            &[],
        )
    };

    let mismatch = format!("checksum mismatch for {NAME}; nothing installed");
    let (ok, text) = install(Some("97.0.0"));
    assert!(!ok && text.contains(&mismatch), "install.sh: {text}");
    let (ok, text) = update(&["--version", "97.0.0"]);
    assert!(!ok && text.contains(&mismatch), "conde update: {text}");

    let (ok, text) = install(None);
    assert!(ok, "install.sh: {text}");
    let (ok, text) = update(&[]);
    assert!(ok, "conde update: {text}");
    assert_eq!(std::fs::read(by_script.join("conde")).unwrap(), release);
    assert_eq!(std::fs::read(&by_update).unwrap(), release);
}
