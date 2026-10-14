# Review of 7ab7215..6f19d1d

Head: `6f19d1dbc89417e27e34c2cba8b469ccc4eeca7e`. Isolated checkout: `/home/kdobmayer/.cache/conde-review/v2-6f19d1d`. One review round, within the requested 30-minute window. No production changes or live-board probes.

Panel status: complete. Four fresh independent Sol lens passes completed before this capstone: claims, regression, security and tests. Claims and tests explicitly retain the browser/execution limits below; parent performed the reported executable verification.

## Findings

### P2 — The newly unconditional static route permits filesystem escape in debug builds

`crates/conde-remote/src/server.rs:359` now mounts `/app/{*path}` even when no web sign-in is configured. `web/app.rs:347` passes the decoded request path into `Assets::get`. RustEmbed 8.13.0's debug implementation canonicalizes that path, but expressly permits an outside-root target when the final requested component is a symlink (`rust-embed-impl/src/lib.rs:213–233` in the installed dependency). Encoded parent segments can therefore escape the asset root and read an external symlink target without authentication.

Reproduced against a new loopback test server with `ServerConfig::default()` (no sign-in): created a temporary text file and symlink outside web-dist, requested an encoded parent traversal ending at that symlink, and received **HTTP 200 with the exact benign fixture contents** instead of 404. On Linux, process file-descriptor symlinks also make open database files a relevant target; no such private files were read in this review. Release builds use exact embedded-name lookup and are unaffected. The preexisting public asset handler had this weakness when web sign-in was configured; this delta newly exposes it on servers without that configuration.

Fix: validate decoded asset names before lookup, rejecting parent/root components and separator variants, and guarantee asset lookup cannot follow a symlink outside its root; alternatively use embedded-only lookup in debug builds too. Add the negative no-sign-in traversal test alongside positive static asset checks.

### P2 — Timestamp search loses its cutoff on the advertised next page

`crates/conde/src/ops/messages.rs:330–336` makes the timestamp predicate and numeric cursor mutually exclusive. At `:363` it returns only a sequence cursor, and `surface.rs:211–216` tells CLI/MCP callers to continue using that number. Following the hint clears `since` and allows older matching messages into later pages.

This is a supported data shape: `docs/DESIGN-v2.md:747–750` explicitly permits copied DMs older than imported kond DMs to be appended after them; `history.rs:1688–1695` preserves their original dates. The real SQLite probe inserted 200 qualifying messages, an older copied message, and one more qualifying message. The timestamp search returned 200 hits; following its cursor returned **the older message plus the qualifying message**, where only the latter satisfies the cutoff.

Fix: carry the original cutoff through continuation, using a separate time predicate alongside the sequence cursor or a composite cursor, and update CLI/MCP hints accordingly. Preserve existing numeric-cursor compatibility.

### P3 — Resumed migration silently omits its local-binding move report

`crates/conde/src/daemon/publish.rs:348–357` can finish a pending local migration from `board.info`, but returns only `BoardInfo`. `setup.rs:831–840` then sees `mailbox_only`, prints the generic “is on v2” line and returns. It never reaches the move renderer at `setup.rs:976–984`.

Trigger: an unanswered initial migration leaves a pending operation; after the board recovers, `conde boards --migrate <alias>` applies its manifest and moves an eligible old local binding. The stored `moved` data exists, but the human does not see which name was unbound. This contradicts the new setup-output promise at `docs/DESIGN-v2.md:682–684`.

Fix: expose and render the resumed operation's stored move report before setup exits. This finding is verified from producer/consumer control flow, not an executed end-to-end recovery probe.

## Validation

- `npm test`: 5 passed. `npm run check`: 0 errors, 0 warnings. `npm run build`: passed.
- `npm run icons` followed by a tracked diff check regenerated identical SVG and PNG assets. The committed mark test itself checks only SVG freshness.
- `cargo test -p conde --lib --test cli_wiring --test mcp --test setup --test web_ui -- --nocapture`: 123 substantive Rust tests passed (66 library, 15 CLI, 9 MCP, 18 setup, 15 SQLite web). The 15 PostgreSQL web cases returned early because `CONDE_TEST_PG` was unset; they were not exercised, despite the harness printing “ok”.
- Additional no-sign-in HTTP probe: ordinary favicon/mark assets succeed without a session; session, attachment, event and asset-prefixed dynamic paths remain unavailable; unauthenticated RPC remains rejected; ordinary non-symlink traversal is rejected. Passed.
- Additional external-symlink HTTP probe: failed as described in P2, with 200 instead of 404.
- Additional timestamp-pagination SQLite probe: failed as described in P2, returning the older copied message on continuation.

Scratch probe sources are attached and retained in `6f19d1d-probes/` beside this report. They modify only the isolated review checkout. The first HTTP scratch run needed a rustls provider installed in the test fixture; that fixture error was corrected before the reported probes.

## Rejected candidates and limits

- Publication widening by the automatic move: checks cover all publication boards and pins before unbinding another local name; publication creation remains limited to published rows in the confirmed manifest. Address lookup remains owner-scoped.
- Partial writes on collision: final-binding checks and mutations share one local transaction, and blocked cases return before writes.
- Connecting past pending local apply: `board.info` finishes it first; the adoption helper refuses to connect while a local op is pending.
- Dynamic API exposure through the static route: source separation and the direct no-sign-in probe refute this; the filesystem-backed debug lookup is the specific exception above.
- Inline SVG injection: the unescaped mark is fixed shipped path data, not caller-controlled content.
- Two manifest names ending at the same session: the old apply already permitted that state; not introduced by this delta and excluded as a regression finding.
- Numeric MCP cursors remain accepted at runtime. No concrete shipped strict-schema consumer failure was established.

No release-image execution, live rollout, PostgreSQL execution, or real-browser visual acceptance is claimed. The release lookup distinction is source-verified. The owner retains favicon/header appearance acceptance.

What would make me reject this? The two reproduced P2 behaviors above. No P1 was found; the P3 is an output-contract defect. This is the requested single pass, not a request for another review round.
