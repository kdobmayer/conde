//! The Windows installer and the release workflow agree on names, paths and the
//! integrity contract (the scripts themselves run on Windows and the release runners).

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
fn the_release_builds_and_publishes_the_windows_binary_and_installer() {
    for needle in [
        "runs-on: windows-latest",
        "TARGET: x86_64-pc-windows-msvc",
        "RUSTFLAGS: -C target-feature=+crt-static",
        &format!("NAME: {WINDOWS_BINARY}"),
        "name: conde-windows-x86_64\n",
        "needs: [web, macos, linux, windows]",
        "cp dist/install.ps1 out/install.ps1",
        "-n install.ps1 -f out/install.ps1",
    ] {
        assert!(RELEASE.contains(needle), "release.yml lacks {needle:?}");
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
    let install = || {
        let out = std::process::Command::new("sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../dist/install.sh"))
            .env("CONDE_DIST_URL", format!("file://{}", dist.display()))
            .env("CONDE_INSTALL_DIR", &bin)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
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
}
