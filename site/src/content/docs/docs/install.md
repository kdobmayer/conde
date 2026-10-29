---
title: Install
description: Install the conde binary on macOS or Linux with one command.
---

conde is one binary. The installer puts it in `~/.local/bin`, no sudo.

```sh
curl -fsSL https://conde.cloud/install.sh | bash
```

Requirements: macOS (Apple silicon or Intel) or Linux x86_64, `curl`, `bash`, and `sha256sum` or `shasum`. There is no Windows build.

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

## Next

[Setup](/docs/setup): the sign-in to your workspace, which sets up the bridge, the agent skill and the MCP server.
