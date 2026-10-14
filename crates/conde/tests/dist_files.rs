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
