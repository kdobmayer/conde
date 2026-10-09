# conde distribution

A teammate gets conde with three commands:

```sh
curl -fsSL https://conde.nativeai.space/install.sh | sh
conde login          # or: conde enroll --server https://conde.nativeai.space
conde setup
```

`install.sh` puts the binary in `~/.local/bin` (`sh -s -- --system` installs to
`/usr/local/bin` with sudo). `conde setup` installs the bridge as a user
service, the conde skill for Claude Code and Codex, registers `conde mcp` as an
MCP server in both, and asks before it sets the Claude receiver policy
(`crossSessionInbound`).

## Files

Releases live in the Azure Blob container `dist` of the storage account
`stconde`: anonymous blob read, no listing. Base URL:
`https://stconde.blob.core.windows.net/dist`. Every release overwrites the same
names:

| File | Content |
|------|---------|
| `conde-darwin-arm64`, `conde-darwin-x86_64` | macOS binaries (Apple silicon, Intel) |
| `conde-linux-x86_64`, `conde-linux-aarch64` | static Linux binaries (musl) |
| `SHA256SUMS` | the integrity contract: one `sha256sum` line per published file (binaries, `install.sh`, `latest`); `install.sh` checks its binary's line with `shasum -a 256 -c` (macOS) or `sha256sum -c` (Linux) and refuses a mismatch, a missing line or a missing file |
| `<binary>.sha256` | per-binary `sha256sum` line, kept for manual checks |
| `install.sh` | the installer, same as `dist/install.sh` in this repo |
| `latest` | one line, the released version (`v0.1.0`) |

## Server routes (for the server owner)

The server is stateless and serves no files itself. It answers two public,
unauthenticated routes with a redirect into the container. Both sit outside
`/v1` and need no session.

| Request | Response |
|---------|----------|
| `GET` or `HEAD /install.sh` | `302 Location: $CONDE_DIST_BASE_URL/install.sh` |
| `GET` or `HEAD /dist/<file>` | `302 Location: $CONDE_DIST_BASE_URL/<file>` |

- `CONDE_DIST_BASE_URL` is set on the Container App
  (`https://stconde.blob.core.windows.net/dist`, no trailing slash). When it is
  unset, both routes answer 404.
- `<file>` must match `^[A-Za-z0-9][A-Za-z0-9._-]*$` (one path segment, no
  `..`, no slash, no query passed through); anything else is 404. The server
  does not check that the blob exists; the container answers 404 itself.
- Send `Cache-Control: no-store` on the redirect, so a new `latest` or binary
  is seen at once.
- `curl -fsSL` follows the redirect; the checksum comes from the same
  container, so the redirect target is the trust anchor for both. TLS on both
  hops protects them in transit.

## Release

`.github/workflows/release.yml` runs on a `v*` tag and on manual dispatch. It
builds the two macOS targets on a `macos-15` (arm64) runner, `x86_64` through
`rustup target add`, and the two musl Linux targets on `ubuntu-latest` with
`cargo zigbuild`. It writes per-binary sha256 files and smoke-runs each native binary.
The `publish` job runs in the `release` environment and checks the files
against their sha256, adds `install.sh` and `latest`, and writes
`SHA256SUMS` over every published file. It logs in to Azure
through OIDC, and runs `az storage blob upload-batch --overwrite` into
`stconde/dist`. A tag must equal `v` plus the version in
`crates/conde/Cargo.toml`. The login reads the repository variables
`AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `AZURE_SUBSCRIPTION_ID`. The
federated credential is bound to the `release` environment.

`dist/release.sh` is the local, Linux-only path. It builds the same two musl
binaries into `dist/out/`, using `cargo-zigbuild` or `cross` when installed.
Otherwise it uses plain cargo when the musl target's standard library and a C
compiler for it exist; rusqlite compiles SQLite from C. It reports which
targets it could not build and why. Upload from `dist/out/` by hand with the
same `az storage blob upload-batch` command.

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
