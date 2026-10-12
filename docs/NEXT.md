# conde — after v1 (owner's ask, 2026-10-09: take these if the program finishes early)

Design pass by conde-astra (kond 01M4ETN9K2) applied below; dissent recorded.

## A. Self-service enrollment: Entra SSO in the web UI, API key per host

MOVED INTO V1 as epic E8 (owner, 2026-10-09: Entra exists within hours). The
design below stands; only the sequencing changed.

Owner's shape (2026-10-09): a teammate signs in to the web UI with the
company Entra identity, gets a host key, and the bridge on their machine holds
it. The key is design v1's per-host token, bound to that user, so every
Claude/Codex session that bridge discovers is attributed to them. Revoking
the key kills that host.

Pieces (fit design v1 §3; schema additions named):
- Web login: Entra OIDC authorization code + PKCE through an OIDC library
  (issuer, audience, state, nonce validated). User identity = validated
  (tid, oid) mapped to the existing user_id; never email or display name
  (those are shown, not keyed). Admin-created v1 users are bound to their
  (tid, oid) explicitly once, to avoid duplicate owners. Access = tenant
  allow-list first; group/Graph checks only when needed. Browser sessions
  expire; CSRF protection on key creation, revocation and sends. A browser
  human acts as an authorized user actor without a host credential.
  Disabling a user disables browser and every host key; Entra logout is not
  revocation.
- Keys page: list / create (shown once) / revoke; each key = one host with a
  label; revocation also terminates that host's live bridge connection.
- CLI, default (owner's call 2026-10-09): `conde login --server <url>` opens
  the browser on the login page with a one-time code; after SSO the server
  mints the host key; the localhost redirect carries only a single-use,
  short-lived code bound to that CLI run (state + PKCE), and the CLI redeems
  it over TLS. The key itself never appears in a URL or browser history.
  Astra's dissent: a second client flow before a demonstrated need; the
  paste below already covers every host. Kept because the owner wants it
  and the cost is small; the paste path is the fallback and ships first.
- CLI, fallback (servers, remote shells): the keys page creates a key and
  `conde enroll` reads it at a masked prompt. Never on argv or in env.
- Until Entra exists (v1 tonight): keys are minted by an admin command on
  the VM. Same table, same bridge path, so the web flow is additive.
- Entra side (dakai-helper, 2026-10-09): single-tenant app in tenant
  2e51b484-b008-4fc0-8f70-555c1317b555, assignment-required enterprise app
  as the allow-list (Krisztián and Dávid Pacsuta for now), redirect
  https://conde.nativeai.space/auth/callback and the loopback redirect for
  `conde login`, scopes openid profile email, env ENTRA_TENANT_ID,
  ENTRA_CLIENT_ID, ENTRA_CLIENT_SECRET (vault), ENTRA_REDIRECT_URI. No
  public client flows; no device code.
- Teammates in another tenant need guest accounts in this tenant.
- Cost: one Opus lane (web login, keys page, enroll prompt, conde login);
  app registration by dakai-helper.

## B. Web UI (same dispatcher over HTTPS `/v1/rpc`)

Value is reach and visibility, not visuals. Order by the stated need (a human
on a phone reaches an agent): conversation + send first, then fleet polish.
1. Conversation: a DM or channel in chronological order with reply links
   (nested thread UI deferred), attachments downloadable, and per message
   its delivery state, retry clock and last error inline.
2. Send box: message an agent or channel; mentions autocomplete from the
   fleet.
3. Fleet (the owner's favourite): agents per owner and host, pinned roles,
   online = unexpired bridge heartbeat, with discovery (session seen by the
   bridge) shown separately from verified reachability (last delivered), and
   held/stuck badges per agent.
A separate delivery view is deferred; its data lives on 1 and 3.
Login = the Entra session from (A). Frontend: Svelte (SvelteKit, static
adapter; owner's choice 2026-10-09, never React or Angular), built to static
files that the Rust server serves next to /v1/rpc and the WebSocket. One
service, one container; no separate frontend deployment.

Versions (owner's rule 2026-10-09: check the live index, pick the best fit,
not a remembered one). Checked 2026-10-09 on npm: svelte 5.57.2, @sveltejs/kit
3.0.1 (3.0 released 2026-10-06), @sveltejs/adapter-static 4.0.0 (2026-10-01),
vite 8.0.0 (2026-10-08), node 22 installed. Decision (owner + lead, 2026-10-09): the SvelteKit 3 line. The 3.0 release
candidate has been public since 2026-08-13, 3.0 stable since 2026-10-01, 3.0.1
since 2026-10-06; its breaking changes (config in the Vite plugin, #lib
alias, $app/state, renamed helpers) only concern migrations from Kit 2 and a
new app never meets them; adapter-static 4 and Vite 8 match; Node 22.17+ and
TypeScript 6 required (node 22.20 installed). Pin exact versions, bump once
at lane end. Re-check the index at lane start. Rust side: every crate is at
crates.io latest as of 2026-10-09; container image builds on rust 1.98.

Order: A first (its keys page is the enrollment surface), then B 1-3, then C.
Neither starts before conde v1 is READY.

## C. Remote MCP endpoint (Streamable HTTP, MCP 2026-07-28) on the server

For clients with no bridge: a human in Claude desktop/web or on a phone
acting as themselves (actor = user, never a session), and Claude Code cloud
sessions (`claude --cloud`, Anthropic-hosted VMs: no machine of ours, so no
bridge can push into them; they poll through this endpoint with the human's
token). Orca (onorca.dev) needs nothing: it runs the local CLIs in worktrees
on the user's machine, which the bridge already covers; an SSH worktree host
needs its own bridge and host key. Auth = the Entra OIDC
login from (A) as the OAuth authorization server the MCP spec expects; tools =
the human subset of `conde mcp` (send, read, inbox, agents, search, react).
Same dispatcher; no identity claims in tool arguments; no push (the client
polls inbox or reads threads). Not a replacement for the local bridge + stdio
MCP, which keep session identity and push on the machine. After A.

## D. Images for the web UI (owner's ask, 2026-10-09)

Every image (logo, favicon, empty-state illustration) is generated through
conde-astra's image generation skill (it calls a Codex session); the UI lane
asks Astra on kond with the brief below, never generates on its own.

Brief, always included: no AI tells. Forbidden: saturated neon or
purple-to-teal gradients, glow, lens flares, glossy 3D blobs, dramatic rim
lighting, perfect mirror symmetry, hexagon / node-network / circuit-brain
motifs, hyper-detailed backgrounds, rendered text. Wanted: one simple
geometric mark, two or three muted colours taken from the UI palette, flat
fill, no gradient, legible at 16 px, a monochrome variant, SVG where possible.
Ask for at least three options; judge them rendered in the real page at real
size (favicon, header, dark and light theme) before accepting one.

## E. Windows support (owner, 2026-10-09: last, only after everything else; one user)

Scope, from what the code assumes today: x86_64-pc-windows-msvc build on a
windows-latest runner (conde.exe + SHA256SUMS); install.ps1 (irm | iex) to
%LOCALAPPDATA%\conde\bin with PATH update; the local CLI transport (unix
socket today) becomes a named pipe or Windows AF_UNIX with the same NDJSON
frames and a peer check; the bridge service as a scheduled task at logon (or
a Windows service); Claude Code's Windows local transport differs (the
cc-socks auth line is required only there, verify the pipe path and the
session record on a Windows install of the current Claude Code); Codex
app-server path on Windows; identity ancestry walk via the Windows process
API (CreateToolhelp32Snapshot / NtQueryInformationProcess) behind the same
ProcessTable injection; credentials under %APPDATA%\conde with ACL-restricted
file; `conde setup` writes the Claude/Codex config at their Windows paths.
Acceptance: the one Windows user runs the README path with desk watching.

Status 2026-10-10: implemented, not yet run on Windows. The acceptance run is
the first execution of every Windows code path.

Verified here, without a Windows machine:
- `cargo check --workspace --tests --target x86_64-pc-windows-msvc` passes
  (official rust 1.95.0 with its Windows std; the C parts of ring and SQLite
  are not compiled by a check, the release runner compiles them). Tests that
  drive the daemon over a unix socket or check unix modes are `cfg(unix)`.
- Unit tests on Linux behind the injection points: the pipe-name parser and
  the key-file hash (ported from Claude Code's), the FILETIME/.NET-ticks start
  rule, receipts gated on the auth line, the owner-only SDDL, the SID peer
  check, the Windows paths, the Windows `ProcessTable` (exited and reused
  parent pids, npm-launched harnesses), `conde setup` on Windows (task XML in
  UTF-16 with CRLF, the schtasks sequence on first run, rerun, binary move and
  stale bridge, the Claude/Codex config at `%USERPROFILE%`, `--check`), the
  `/install.ps1` redirect, and install.ps1's text against the workflow.
- install.ps1 parses in PowerShell 7.6.6 and, on Linux against a local file
  server, runs `irm | iex` through download, version, checksum match, mismatch,
  missing line and missing SHA256SUMS (it cannot run the exe or edit the
  registry there).
- Unix behaviour unchanged: the full workspace test run.

Read from the shipped bundles, unverified on a Windows install (Claude Code
2.1.296 and Codex 0.162.1 win32-x64 packages, read statically):
- Claude Code's Windows inbox is the pipe `\\.\pipe\LOCAL\cc-msg-<32 hex>` in
  `messagingSocketPath`, the record carries `procStartFt` (creation FILETIME;
  `procStart` .NET ticks only without its native helper), the key file is
  `<pid>.<sha256 of \\.\pipe\ + lowercased name>.key`, and the auth line is
  required there. It answers receipts only to a `from` pipe named
  `cc-msg-<32 hex>` that a key file vouches for, opening each with the auth
  line; conde's bridge binds such a pipe and publishes that key, and sends
  only to a pipe whose server pid is the session's. The `uds:\\.\pipe\...`
  address form is accepted: the address character class in the 2.1.296
  strings includes the backslash.
- `\\.\pipe\LOCAL\` is session-scoped: the bridge must run in the human's
  desktop session, so the task uses an interactive logon token.
- Codex's app-server control endpoint is the same
  `.codex\app-server-control\app-server-control.sock`, an AF_UNIX socket
  (Codex links `uds_windows`); conde connects through socket2 and tests the
  file without following its reparse point. `codex app-server daemon start` on
  Windows is assumed from the bundled daemon crate.

Waits for the first Windows run:
- `conde setup`: `schtasks /Create /XML` with a UTF-16 file, registration
  without elevation, the task starting at logon, no 72 h limit, the console
  detaching to `%LOCALAPPDATA%\conde\data\conde.log` (a flash at logon);
  `/End` is a hard stop; the state line reads English `schtasks` labels only.
- The CLI pipe `\\.\pipe\conde-<user>`: owner-only DACL, remote clients
  rejected, the daemon's check of each client's token user and the client's
  check of the server's (a squatted name is refused, not used). Not
  verifiable by design: any process of the same user is trusted, as with the
  uid check on Unix.
- Secret files under `%APPDATA%\conde`, and the data directory, created with a
  protected DACL for the current user only; load and doctor accept a DACL whose
  allow entries name only the current user's SID, SYSTEM and Administrators.
  The file's owner is not read, as on Unix, where only the mode bits are.
- The Toolhelp ancestry walk, command lines via NtQueryInformationProcess, the
  echo-off console prompt, the host name from GetComputerNameExW.
- install.ps1 in Windows PowerShell 5.1: the user PATH registry edit and its
  broadcast, renaming a running `conde.exe` aside; the blob's text content type.
- The release workflow's `windows` job (static CRT build, smoke test, identity
  probe over the pipe) ran green on its first run; it gates `publish` like
  macOS.

Decisions the section left open: a named pipe, not AF_UNIX, for the CLI
(std's Windows `UnixStream` is unstable in 1.95 and tokio has no AF_UNIX on
Windows; tokio's named pipes carry the same NDJSON); the pipe in the global
namespace, protected by its DACL and both SID checks; a scheduled task, not a
service (no admin, runs in the desktop session); the task inherits the logon
environment, so setup names relocated directories to `setx` instead of pinning
them; data in `%LOCALAPPDATA%\conde\data`, config in `%APPDATA%\conde`
(`XDG_CONFIG_HOME` still wins when set); a Windows send without a peer token
fails at once instead of being dropped by Claude Code; the task names the user
by SID (`<UserId>`), since Entra-joined machines name users `AzureAD\...`; a
restart is reported only after `/End`, `/Run` and an answer on the pipe.

Follow-ups for the first Windows run (review 2026-10-10, open):
- Another local user can create `\\.\pipe\conde-<user>` first: the bridge then
  fails to bind, and the task's restart-on-failure retries up to 999 times
  without a message anyone sees.
- An elevated client against a non-elevated bridge: reading the elevated
  process's token for the SID check may be denied, which refuses it.
- FreeConsole when Windows Terminal is the default console host.
- The task's default priority (7, below normal) for the bridge.
- A `/End` hard stop leaves the reply pipe's `.key` file in the sessions
  directory until Claude Code sweeps keys of dead pids.

## F. Session discovery beyond ~/.claude (Orca multi-account) — DROPPED 2026-10-09

Owner decision after asking Balázs's agent: his Orca sessions run the default
~/.claude account (CLAUDE_CONFIG_DIR unset), notifications, skill and MCP all
worked in an Orca-launched session. Not needed; revisit only if someone uses
Orca's multi-account mode.

Orca (onorca.dev) launches Claude Code with --dangerously-skip-permissions by
default ("yolo"; manual/mixed selectable), so README step 3 (crossSessionInbound
accept) is required there. For a non-default Claude account Orca injects its own
CLAUDE_CONFIG_DIR, so those sessions publish their session files, settings,
skills and MCP under that directory and the bridge does not see them. Fix: the
bridge discovers Claude sessions from the running processes (walk the process
table for claude binaries and read each one's CLAUDE_CONFIG_DIR from its
environment, or watch every sessions dir named in a config list), and
`conde setup` offers to install the skill/MCP/setting into every config dir it
finds. Until then: system-default account works; injected dirs do not.

## G. Found at the homepc switch (2026-10-09), small

- DONE 9ea75a5: `conde search --after` takes an RFC 3339 time (or conde's own form, or the listing minute) as well as the cursor, CLI and MCP.
- DONE 7a84ebc: the migration apply moves the session off a free local name (unpinned, unpublished, not in the manifest; history kept) or refuses before any write with the exact unregister command; board.info no longer connects past a pending local apply.
- The offline gate (G1 on the real machine, network blocked for 30 s) was not run at the switch: run it once with the owner present.

## H. From the first v2 installs (2026-10-09)

- Headless hosts: `conde login --no-browser` makes the human finish the loopback callback by hand from another device. Add a pasted-code path (the keys page shows a one-time code; `conde login --code` redeems it) so a headless host needs no callback.
- `conde setup` skips the skill/MCP step when an MCP server named `conde` is already registered for another instance; it should detect that and register under a second name (`conde-dakai`), pointing the skill at it.
- LANDED d815a52, 88c8c02; review P2s fixed in the next batch (confirm page before the code, a mistyped code keeps the waiting login, CONDE_SOCK pinned in every MCP entry and the second instance's skill under skills/conde-<alias>). Follow-ups from that review:
  - A pasted-code confirm by another account is refused with 403 before the record's expiry is checked: an expired login of someone else still blocks the link (harmless, but the message should say "expired, run conde login again").
  - The code issue takes the pending record and inserts the new one in two store transactions: a concurrent confirm or redeem in between can hit the primary key and answer 500. One store call that replaces the record in one transaction fixes it.
  - Redeem has no rate limit beyond the 5 tries per issued code; a client that re-issues (the same account confirming again) resets the budget. Add a per-IP or per-account limit on /auth/cli/redeem like the OAuth endpoints'.
  - `conde login --code` without a terminal exits 0 after printing the URL; a script cannot tell "started, waiting for the code" from "enrolled" by the exit status. Consider a distinct exit code or a machine-readable line.

## I. Remote MCP, from the security review 2026-10-10 (docs/remote-mcp-security-review.md)

- Refresh reuse revokes strictly: if the live check shows Claude refreshing in parallel, a token consumed less than 10 s ago answers `invalid_grant` without revoking.
- Client metadata trust is per host: pin the exact client_id URLs once the hosted one is seen in the log.
- No RFC 7009 revocation endpoint: a connector removed in Claude stays on the keys page until it expires.
- `search` with a never-matching pattern scans all visible history, bounded only by the 120/min bucket.
- A sign-in `next` path over 2048 characters is dropped silently: a very long authorize request lands on /keys.
- A code replayed between its take and the grant's creation (milliseconds) finds no grant to revoke yet, so the grant made by the first redemption stays live; closing it needs the take and the create in one transaction, or a tombstone the create checks.
- From the review of the token purge (purge on every refresh and in an hourly sweep, landed 2026-10-11): no test runs the purge after a refresh-token replay; no test shows the sweep ends at shutdown; the sweep task is spawned detached in `server::start`, so the server's handle does not wait for it.

## J. From the NUC run 2026-10-11

Fixed in this batch: the build id in `conde --version`, `conde doctor` and `conde setup --check`; the `re-enrolled host` line and one keys-page row per host; `conde setup` skipping Claude Code and Codex where `--check` does; a new key from `conde login` or `conde enroll` reaching the running bridge without a restart (`board.reload`, and a reread of the board file when the board refuses the key). Found on the way, not fixed:

- `conde admin key create` (`ops::hosts::insert_key`) adds a key without revoking the host's live ones. It is the only way to get a second live key for a host: the login redeem, the keys page and the admin API revoke and insert in one transaction, and Postgres writes are serialized by the writer advisory lock. Decide whether it revokes too, or stays as the overlap path for a hand-over rotation.
- `conde doctor` prints the build of the binary it runs, not of the running bridge. A reinstall replaces the file under a running service, and setup does not restart it for that. Doctor could show the daemon's build id and flag a mismatch.
- Until the server runs this batch, its redeem answer has no `reenrolled` field and the CLI prints `enrolled host` for a re-enrollment too.
- A local build's id names HEAD and the date of the first build of that commit: the build script reruns only when HEAD moves, so a later rebuild keeps the older date, and uncommitted changes are not marked.
- `reenrolled` in the redeem answer means the host existed, not that a live key was revoked: a host whose keys were all revoked earlier still gets "the previous key for this host is revoked". The mint knows `replaced` (live keys revoked); the CLI could say which.
