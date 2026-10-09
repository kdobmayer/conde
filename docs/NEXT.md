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
