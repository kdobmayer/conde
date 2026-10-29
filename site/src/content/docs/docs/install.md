---
title: Install
description: Install the conde binary on macOS, Linux or Windows with one command.
---

conde is one binary. The installer puts it in `~/.local/bin`, no sudo.

```sh
curl -fsSL https://conde.cloud/install.sh | bash
```

Requirements: macOS (Apple silicon or Intel) or Linux x86_64, `curl`, `bash`, and `sha256sum` or `shasum`. On Windows, see [Windows](#windows).

## What the installer does

1. Detects the OS and CPU and picks `conde-darwin-arm64`, `conde-darwin-x86_64` or `conde-linux-x86_64`. An x86_64 shell under Rosetta gets the native Apple silicon build.
2. Reads the current release version, downloads the binary and `SHA256SUMS`, and checks the binary against its line. On a mismatch, or when the line is missing, nothing is installed.
3. Runs the downloaded binary once (`--version`) to make sure it works on this machine.
4. Moves it to `~/.local/bin/conde` in one rename, so a running conde is replaced atomically.
5. Prints the next command: `conde login` with your workspace's address.

The installer refuses to run under `sh`; it needs bash. The script is plain text at [conde.cloud/install.sh](/install.sh), so read it before you run it.

## Check

```sh
conde --version
```

Expected: `conde 0.1.0 (<commit> <date>)`, the version, the release's commit and the build date.

If the installer said `~/.local/bin is not on your PATH`, add the line it printed to your shell's startup file, then open a new terminal, or run this in the current one:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## Options

The installer reads three environment variables.

| Variable | Default | Meaning |
|---|---|---|
| `CONDE_VERSION` | the current release | Install a specific release, with or without the `v`. |
| `CONDE_INSTALL_DIR` | `~/.local/bin` | Where the binary goes. |
| `CONDE_DIST_BASE_URL` | the release bucket | Where the binaries come from, for a mirror or a self-hosted release. |

```sh
CONDE_VERSION=0.1.0 bash -c "$(curl -fsSL https://conde.cloud/install.sh)"
```

## Upgrading

```sh
conde update
conde setup
```

`conde update` downloads the latest release for this machine from the same files the installer reads, checks it against its `SHA256SUMS` line and renames it over the running binary, so a failed download or check leaves the old binary in place. It prints `conde <old> -> <new>`, or `conde <version> is current`, and restarts the bridge service when that service runs the binary it just replaced; a service that runs another conde is left alone, with the hint to run `conde setup`. `--check` only reports; `--version 0.3.0` installs a specific release. `CONDE_DIST_BASE_URL` works as for the installer.

Then `conde setup` rewrites what the new release changed (the skills, the MCP entries, the service file). Your enrollment, names and messages stay; no sign-in is needed. A conde without `conde update` (0.2.0 and older) upgrades by running the installer again.

## macOS

`curl` sets no quarantine attribute, so Gatekeeper lets the binary run. If macOS still says it cannot be opened, or the shell prints `killed: 9`, clear the attribute and sign the binary ad hoc:

```sh
xattr -d com.apple.quarantine ~/.local/bin/conde 2>/dev/null; codesign --sign - --force ~/.local/bin/conde
```

## Windows

In PowerShell (Windows PowerShell 5.1 or PowerShell 7), no administrator rights needed:

```powershell
irm https://conde.cloud/install.ps1 | iex
```

Requirements: Windows 10 or 11 on x64, or on Arm64 through its x64 emulation. The binary links the C runtime statically, so no Visual C++ redistributable is needed.

The installer downloads `conde-windows-x86_64.exe` and `SHA256SUMS` from the same release as the other systems, checks the binary against its line, runs it once, and puts it at `%LOCALAPPDATA%\conde\bin\conde.exe`. It adds that directory to your user PATH: the PowerShell window that ran it has it at once, other terminals once you open them again, and running Claude Code and Codex sessions once they restart. It reads the same three variables as the shell installer (`$env:CONDE_VERSION = '<version>'` before the line picks a release; the first with a Windows binary is the one after 0.5.4). The script is plain text at [conde.cloud/install.ps1](/install.ps1).

On an upgrade the installer moves the running `conde.exe` aside (a bridge that runs it keeps it locked until it exits) and names the next commands, as on the other systems. `conde update` works the same way, and restarts the scheduled task onto the new binary.

`conde setup` (and `conde login`, which runs it) installs the bridge as the scheduled task `conde`, which runs `conde daemon` as you at your logon, in your desktop session. Its log is `%LOCALAPPDATA%\conde\data\conde.log`, next to the local board; the board files are under `%APPDATA%\conde`.

## Next

[Setup](/docs/setup): the sign-in to your workspace, which sets up the bridge, the agent skill and the MCP server.
