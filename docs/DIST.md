# conde distribution

A teammate gets conde with one install line, then `conde login`, which also sets the machine up:

```sh
curl -fsSL https://conde.cloud/install.sh | bash          # macOS, Linux
irm https://conde.cloud/install.ps1 | iex                  # Windows (PowerShell)
conde login --server https://<workspace>.conde.cloud
```

`install.sh` puts the binary in `~/.local/bin`; `install.ps1` puts `conde.exe` in
`%LOCALAPPDATA%\conde\bin` and adds that directory to the user PATH. Both read `CONDE_VERSION` (a
release, default the one `latest` names), `CONDE_INSTALL_DIR` and `CONDE_DIST_BASE_URL`, check the
binary against its `SHA256SUMS` line, run it once, and print the same `Next:` lines. `conde update`
reads the same files.

## Files

Releases live in the public GCS bucket `conde-cloud-dist`, base URL
`https://storage.googleapis.com/conde-cloud-dist`:

| File | Content |
|------|---------|
| `latest` | one line, the current release (`v0.5.4`) |
| `install.sh`, `install.ps1` | the installers, as `dist/` in this repo; both `text/plain; charset=utf-8` (`irm \| iex` reads install.ps1 as text) |
| `v<version>/conde-darwin-arm64`, `conde-darwin-x86_64` | macOS binaries (Apple silicon, Intel) |
| `v<version>/conde-linux-x86_64`, `conde-linux-arm64` | the static Linux binaries (musl; x86_64 and arm64) |
| `v<version>/conde-windows-x86_64.exe` | the Windows x64 binary (MSVC, C runtime linked statically, so no Visual C++ redistributable is needed) |
| `v<version>/SHA256SUMS` | the integrity contract: one `sha256sum` line per binary; an installer refuses a mismatch, a missing line or a missing file |

`latest` and the installers are written with `Cache-Control: no-store`. A published version is never
overwritten: the publish job refuses a version whose `SHA256SUMS` exists.

https://conde.cloud serves `/install.sh` and `/install.ps1` from `dist/` (the site build inlines
them; `site.yml` deploys on a change).

## Server routes (for the server owner)

A workspace server serves no release files itself. It answers three public, unauthenticated routes
with a redirect into the bucket, outside `/v1`:

| Request | Response |
|---------|----------|
| `GET` or `HEAD /install.sh` | `302 Location: $CONDE_DIST_BASE_URL/install.sh` |
| `GET` or `HEAD /install.ps1` | `302 Location: $CONDE_DIST_BASE_URL/install.ps1` |
| `GET` or `HEAD /dist/<file>` | `302 Location: $CONDE_DIST_BASE_URL/<file>` |

- `CONDE_DIST_BASE_URL` is the chart's `distBaseUrl` (the bucket, no trailing slash). When it is
  unset, the routes answer 404.
- `<file>` must match `^[A-Za-z0-9][A-Za-z0-9._-]*$` (one path segment, no `..`, no slash, no query
  passed through); anything else is 404. The server does not check that the file exists.
- The redirect carries `Cache-Control: no-store`, so a new `latest` is seen at once.

## Release

`.github/workflows/release.yml` runs on a `v*` tag and on manual dispatch (which builds and tests
only). A tag must equal `v` plus the version in `crates/conde/Cargo.toml`. It builds the two macOS
targets on `macos-15`, the musl Linux target with `cargo zigbuild` on `ubuntu-24.04`, and
`x86_64-pc-windows-msvc` on `windows-2025`; each native binary must print this build's commit and
date, and on macOS the identity probe runs (a daemon on a temporary socket, `whoami` as the human,
a refusal under a process named `claude`). The Windows build steps are the ones `ci.yml`'s
`windows-install` job runs before it installs that binary with `install.ps1` from a local copy of
the bucket layout, upgrades it, and runs `conde setup` and `conde doctor` with it; a test
(`crates/conde/tests/dist_files.rs`) keeps the two identical.

The `publish` job runs in the `release` environment, checks every binary against its sha256, writes
`SHA256SUMS`, logs in to Google Cloud through GitHub OIDC and uploads `v<version>/`, then `latest`,
`install.sh` and `install.ps1`. The server image is built and promoted by the `image` and `promote`
jobs (see `docs/DEPLOY.md`).

`dist/release.sh` is the local, Linux-only path: it builds the musl binary into
`dist/out/v<version>/` with `SHA256SUMS`, plus `latest` and both installers in `dist/out/`, laid
out like the bucket, using `cargo-zigbuild` or `cross` when installed, else plain cargo when the
musl standard library and a C compiler for it exist (rusqlite compiles SQLite from C).

## Windows notes

- The bridge is the scheduled task `conde` (`conde setup` registers it with `schtasks /Create /XML`
  from `%LOCALAPPDATA%\conde\conde-task.xml`): `conde daemon` at the user's logon, in the desktop
  session, no time limit, restarted on failure. No administrator rights are needed.
- A running `conde.exe` cannot be replaced but can be renamed: `install.ps1` and `conde update` move
  it aside to `conde.exe.<id>.old`, put the new binary in its place, and remove the old copies on
  their next run once nothing runs them.
- The local board and the bridge log are in `%LOCALAPPDATA%\conde\data`, the board files and secrets
  in `%APPDATA%\conde` (owner-only DACL), and the CLI reaches the bridge over the named pipe
  `\\.\pipe\conde-<user>`.

## macOS notes

- `curl` sets no `com.apple.quarantine` attribute, so Gatekeeper does not
  check a binary installed by `install.sh`. A binary downloaded with a browser
  carries the attribute and macOS refuses to open it ("cannot be opened because
  the developer cannot be verified"). Clear it with
  `xattr -d com.apple.quarantine ~/.local/bin/conde`.
- Apple silicon runs only signed code. The Rust linker ad-hoc signs arm64
  binaries, which is enough for a binary without the quarantine attribute. If
  the shell prints `killed: 9` at start, re-sign it ad hoc:
  `codesign --sign - --force ~/.local/bin/conde`.
- The binaries are not Developer ID signed or notarized. That is needed only
  for browser downloads without the `xattr` step.
- `conde setup` writes a launchd user agent
  `~/Library/LaunchAgents/space.nativeai.conde.plist` (RunAtLoad, KeepAlive,
  logs in `~/Library/Logs/conde/conde.log`). It loads the agent with
  `launchctl bootstrap gui/$UID`. The agent's environment pins `PATH` and
  `CONDE_SOCK` to the values of the shell that ran setup. A launchd agent need
  not see the shell's `$TMPDIR`, which the default socket path depends on.
  Rerun `conde setup` after you move the binary.
