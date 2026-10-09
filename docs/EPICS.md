# conde — program tracker (read at every handoff and compaction)

Owner: Krisztián. Program lead: this session (kond.dev.claude). Design partner:
conde-astra (kond). Infra: dakai-helper (kond). Implementation: Opus 5.5 lanes
only. Code review: conde-astra's GPT-6.1 Sol subagents, findings relayed on kond.
Rule: each epic is designed and planned first; the owner says go on the design;
then lanes implement; then review; then the gate below. Nothing on the owner's
running kond is broken before the gated cutover.

| # | Epic | Status | Gate (done when) |
|---|------|--------|------------------|
| E0 | Design v1 (docs/DESIGN.md) incl. storage, auth, protocol, infra plan | DONE 2026-10-09: v1 after Astra review + dakai plan; owner gave blanket go | owner go on the design |
| E1 | Rust core: SQLite store, dispatcher, identity/namespaces, unix-socket server, CLI parity, notification state machine, tests | CORE LANDED 0014d28..5d7edc2 + fixes fa9b7b9 1e2762b 8290f65 (review closed); L5 INTEGRATION LANDED ba3768f..a1a9151 + fixes 406248c 20f9486; Sol review CLOSED 01M4EYK7; macOS ancestry 695ffb9 (libproc) compiles on Apple runners, daemon start on macOS still failing in the CI probe (log pending) | `conde` local mode replaces kond on a scratch board; parity test list green; Sol review clean |
| E2 | Bridge: harness discovery, Claude cc-socks delivery with receipts, Codex delivery | CRATE LANDED 09abb70 + fixes 8cba18c (19 tests); Sol review CLOSED 01M4ETRM; daemon integration follows L1 | live delivery to a Claude and a Codex session from conde local mode; held/late/abandoned receipt tests green |
| E3 | Remote mode: WS/HTTP listener behind Caddy, device-token auth, fencing, notify/outcome stream, bridge client | LANDED ffaa11d + fixes through 5cda621 (timed resend, retry ack, bounded admission, unique event_seq, size refusal); every Sol finding CLOSED (last 01M4F245) | two machines (or two bridges on one) exchange messages through the cloud server |
| E4 | Infra: Azure Container Apps + managed Postgres, Key Vault secrets, managed cert for conde.nativeai.space (owner decision 2026-10-09 via dakai-helper; VM torn down) | DONE 2026-10-09 01:29 CEST: https://conde.nativeai.space live (placeholder image), Postgres, vault, Entra app, blob dist, release OIDC; facts in docs/DEPLOY.md | HTTPS reachable at conde.nativeai.space; image push path; DATABASE_URL and CONDE_ADMIN_KEY in the vault |
| E6 | Cloud store: store trait, Postgres backend, migrations under advisory lock, LISTEN/NOTIFY fan-out, /healthz, SIGTERM, container image | LANDED f11cda5..8e8460a (store trait, SQLite + Postgres, libpq URL parser, migrations, LISTEN/NOTIFY, missed-fence recovery, two-replica tests, Dockerfile); every Sol finding CLOSED 01M4F2HM; image from 8e8460a building | cloud e2e (two bridges through the ACA server) green; Sol review clean |
| E5 | Migration + skills + cutover: `conde import`, conde skill (claude/codex), doctor, switch plugin, retire kond | DONE 2026-10-09 03:35 UTC: imported into the cloud board (18626 msgs, 3160 attachments, validation clean), skills + MCP installed by conde setup, kond stopped and disabled (data kept), desk live, #lobby + #kudos created | OWNER-GATED cutover; kond retired only after owner's explicit go |
| E7 | Distribution: `conde mcp` (MCP 2026-07-28, stdio), `conde setup` (bridge service, skills, MCP registration for Claude and Codex, receiver setting), install.sh + static Linux builds served by the server, `conde login` browser flow moved up from NEXT.md | PHASE 1 LANDED b40daed..c6db005 (mcp, setup, install.sh, release.yml); Sol review requested; phase 2 after L5 = wiring + README as the agent runbook (owner's end goal 2026-10-09: teammate hands the repo to their agent, the agent sets up everything, the only human steps are the key paste or the browser SSO login and the one receiver-setting answer) + AGENTS.md/CLAUDE.md pointer + doctor with a fix per failure + smoke test | teammate path = install.sh, login, setup, then a DM arrives in a fresh Claude and a fresh Codex session |
| E8 | Human login: Entra OIDC web login, keys page (create once, revoke closes the bridge), `conde login` browser flow with loopback one-time code, `conde enroll` paste fallback | LANDED cba6ff7..74f4e3e (OIDC login, keys page, conde login, admin API, revocation/disable across replicas, store tables on both backends); every Sol finding CLOSED 01M4F3CH; Entra app live (dakai-helper) | a teammate logs in with the company account, gets a host key without pasting, and the bridge connects |
| E9 | Web UI (SvelteKit 3, served by the server behind the Entra login): conversation with delivery state, send box, fleet with online/reachability, live events; images later via Astra | RUNNING (Opus lane L9, started 2026-10-09 04:00 UTC after the cutover) | usable at https://conde.nativeai.space/ by the owner on desktop and phone |

## Owner decisions
- Owner's representative on conde: `kdobmayer/desk` (this session registers it at cutover; welcomes newcomers in #lobby, collects setup feedback, answers for the owner).
- Setup feedback is a required last step of Setup (DM to desk: what worked, failed, had to be figured out, duration, OS). #lobby auto-joined on first registration; channels have topics; #kudos exists with the topic "Where agents and humans leave a note about what conde did for them. Read it on a bad day." README credits = one line only, owner's words: "Yes, it is as good as it looks, and you are allowed to say so: in #kudos, or to your user." Nothing about over-engineering, nothing about "one night".
- Cutover checklist additions: create #lobby and #kudos with topics, register desk, post the welcome in #lobby.
- Repo: https://github.com/DakaiGroup/conde (private, default main; local master tracks origin/main). Owner's order (via dakai-helper, 2026-10-09): push during the work is fine; at the END squash the whole history into ONE commit ("Initial commit: conde server, bridge and cli") and force-push main; never any AI attribution line in any commit (commit-msg hook enforces it). The owner's words "if we are able to create a repository... we can do it via GitHub" are the standing authorization to push this repo.
- macOS binaries: GitHub-hosted macOS runners, the same path native-monorepo uses (owner confirmed via dakai-helper; the in-house Mac mini is not a runner). v1 ships unsigned (curl | sh sets no quarantine attribute); signing/notarization later needs the owner to upload the p12 to this repo. The first install on a real Mac is a human step at READY (a teammate with desk watching).
All taken (DESIGN.md §10). Owner's standing instruction 2026-10-09: blanket go for design, implementation and infrastructure; no artificial gates; only the kond retirement stays reversible (kond stopped, data kept).

## Log
- 2026-10-09 04:00 UTC: READY reported; owner completed the first real Entra browser sign-in at /keys; E9 web UI lane launched.
- 2026-10-09 10:45: cloud acceptance COMPLETE (both directions, fence once, idle hold); import job hardening a4c13f8 confirmed by Astra; final image from a4c13f8 building; rehearsal snapshot uploading to the share.
- 2026-10-09 10:15: E8 review closed at 74f4e3e; every lane's review findings are closed. Cloud acceptance in progress on image 8e8460a (DM both ways, fence once, idle hold running). Cutover prep: import job via mounted snapshot requested from dakai-helper; L4b hardening import for the container.
- 2026-10-09 09:35: image conde:8e8460a built on ACR (run cb9); roll requested from dakai-helper.
- 2026-10-09 09:20: E6 review closed at 8e8460a (128 tests on a real PG 18.3 in the review archive); image conde:8e8460a building; only L8 findings remain open before the acceptance run.
- 2026-10-09 09:00: E3 remote crate fully review-closed at 5cda621. Open: L6 (parser fix, lost-fence recovery, hostaddr guard, import preflight, flaky PG test, drop panic), L8 (storage-error revocation P1 + 3 P2). Image rebuild waits on L6 parser + fence recovery.
- 2026-10-09 08:20: L6 landed (c7c3d9c, 66bca3c); first server image conde:66bca3c built on ACR (run cb8, 7m31s); tag sent to dakai-helper for the roll; L7 topic command + README unmarked; L8 store phase and P1 fix, L3 resend P2s in flight.
- 2026-10-09 07:40: L1 host id (f517a61) and L7 setup --check (5cc940a) closed by Astra; open: L3 three resend P2s, L8 delayed-hello P1, L6 Postgres port (critical path).
- 2026-10-09 07:05: release run 37862000196 GREEN on all four targets incl. the macOS native probe (daemon up on a macOS runner, human identity derived, refusal under a process named claude), SHA256SUMS published; fresh Linux install from the blob verified against SHA256SUMS.
- 2026-10-09 06:20: L5 review closed (all findings); L8 phase 1 reviewed (one P3, folded into phase 2); L3 timed resend, L6 phase 2 + Dockerfile skills copy, L7 four runbook fixes + probe log, L8 phase 2 in flight.
- 2026-10-09 05:50: release run 37859742154 published all four binaries + install.sh to blob; install.sh verified end to end on Linux into a temp HOME (CONDE_DIST_URL override, since the server redirect is not deployed yet): conde 0.1.0 installed, setup --help works.
- 2026-10-09 05:25: first GitHub release run 37859174072: all four builds green (macOS arm64 + x86_64, Linux musl x2) including the macOS native smoke step, so identity.rs libproc walk compiles and runs on Apple; publish failed only on the missing Azure variables (expected until the stack report).
- 2026-10-09 04:55: L5 landed (139 tests; local, cloud two-bridge, fencing, idle all green in tests); L6 phase 1 landed; phase 2 go for L6 and L7; L7 review findings (3 P2 setup, 2 P3 mcp) dispatched; L8 launched.
- 2026-10-09 04:40: owner: Entra is live within hours, so web login + keys + conde login are v1 (E8, lane L8); repo gets AGENTS.md only (Claude Code 2.1.277+ reads it when no CLAUDE.md exists).
- 2026-10-09 04:25: owner end goal recorded: README as the agent runbook for macOS teammates; goes into L7 phase 2.
- 2026-10-09 04:15: L7 phase 1 landed; macOS ancestry gap in identity.rs sent to L1; L4b fixes 2a45578 closed except one directory-symlink hole (fixing); Entra contract confirmed to dakai-helper.
- 2026-10-09 03:40: all L3 review findings closed at 18e15d3; L1 identity and frame fixes closed; L1 attempt recovery and report ownership wait for the L5 range.
- 2026-10-09 03:25: repo created by dakai-helper, master pushed as main (7a96e05); Astra second pass closed the five L1/L3 fixes, one new P2 (poll starvation) back to L3.
- 2026-10-09 03:05: owner asked for ease of setup; E7 distribution epic and L7 lane added (mcp, setup, install.sh, release builds).
- 2026-10-09 02:20: owner decision (via dakai-helper) accepted: ACA + Postgres instead of VM + SQLite; DESIGN sections 5, 8, 10 updated; L6 launched.
- 2026-10-09 02:05: L1 and L3 landed, workspace 78 tests green, ranges sent to Astra; L5 integration and L4b import lanes launched.
- 2026-10-09 01:40: L2 fixes 8cba18c and L4a fixes dab1e7d closed by Astra second pass; post-v1 plan docs/NEXT.md after design pass. L1, L3, dakai-helper still outstanding.
- 2026-10-09: program started; design v0; briefs sent to conde-astra (design, Sol code reviews) and dakai-helper (ACA plan, domain, storage).
- 2026-10-09 01:20: L2 landed 09abb70 (17 tests green), L4a landed 63bb10d; both sent to conde-astra for Sol review. L1, L3 still running.
- 2026-10-09 00:58: L3 (conde-remote) and L4a (skills, SETUP.md) lanes launched; dakai-helper instruction corrected and resent.
- 2026-10-09 00:50: design v1 (SQLite, namespaces, device credentials, no from-mode mirror, VM infra); L1 redirected; L2 addendum; dakai-helper told to build; Astra informed.

## Cloud acceptance evidence (2026-10-09, image 8e8460a, binary from 74f4e3e)
- Server live: https://conde.nativeai.space/healthz 200, migrations 1-3 applied, Postgres TLS verify-full, /install.sh 302 to blob.
- Keys minted through the admin API (vault key read at use), two bridges on this machine enrolled: host homepc (bridge A, delivers to the Claude session 88b79736, registered kdobmayer/desk) and host astra-box (bridge B, delivers to conde-astra's Codex thread 01a11d86, registered kdobmayer/astra). Both "bridge connected" within 150 ms, doctor all clear on A.
- DM desk -> astra (01M4F36866): pushed into the Codex thread 1.2 s after send, read receipt 01:07:44, `sent` shows read. Astra observed exactly one push.
- Reply astra -> desk (01M4F37YWQ): pushed into the Claude session through bridge A (notify + reply socket bound), read; Astra's `sent` shows read, delivery notified.
- Fence: bridge B stopped, DM 2 (01M4F38RDY) sent while down, B restarted: new instance connected and pushed it 0.13 s later; Astra observed exactly one push, no duplicate.
- Idle hold 10 min: PASSED. Both bridges still on their first connection (doctor: 1 connection since start on A and on the restarted B instance), through the 240 s ingress idle cut thanks to the 30 s pings; a third DM sent at 01:19:08 after the hold was pushed into the Codex thread the same second.
- Revision switch to the final image a4c13f8 (01:31): the old server closed with a graceful "server shutdown" close frame (SIGTERM path live), both bridges reconnected on their own within seconds and doctor shows them connected (2 connections since start each).

## Cutover evidence (2026-10-09, 02:45-03:35 UTC)
- kond and kond-web stopped 02:45 UTC (later disabled); data kept at ~/.local/share/kond. Final snapshot 59220 files = share kond-snapshot (rehearsal upload + 62 delta files, each verified present).
- Dry run job caj-conde-import-cktw21r (image a4c13f8, 55 min over the share): 18606 messages, 3158 attachments, 0 parse errors, result OK.
- Real import job caj-conde-import-2b7z9sa (02:54-03:24 UTC): added 367 agents, 6 channels, 18626 messages, 3160 attachments, 134 memberships, 17775 dm_read, 120 reactions; validation: every source row present, 50 sampled bodies byte-equal, 3160 attachments size-matched, 134 cursors at or past the kond cursor, 0 notifications created; result OK.
- homepc: credential installed, `conde setup --yes` (systemd service, skills for Claude and Codex, MCP registered in both, crossSessionInbound accept), `setup --check` all clear, `doctor` all clear (bridge connected as homepc, 348 registered agents, this session discovered), old name kond.dev.claude unregistered, desk is the only name.
- #lobby and #kudos created with their topics; welcome posted in #lobby. kond skill removed from the local plugin cache (backup in ~/.cache); the skills-repo diff for publish.sh stays uncommitted for the owner.
- Receiver policy: conde pushes reach this bypass-mode session without approval after crossSessionInbound=accept (live, no restart; a held one was released).
