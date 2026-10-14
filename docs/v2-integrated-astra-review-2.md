# Second integrated review: c385bcd..674d78a

Verdict: REQUEST CHANGES. Two P2s remain; rollout should wait. Six independent Sol lenses completed (security, regression, claims, tests, delivery, history/infrastructure integration), followed by parent source verification and synthetic probes. Privacy boundary reviewed first. Follow-up 2dfae90 was narrowly reviewed during this pass and closes the separately reproduced duplicate-completion marker bug.

## Remaining findings by priority

### 1. P2 — A delayed migration can recreate publications after board.forget

Locations: crates/conde/src/daemon/publish.rs:248,356-398; daemon/board.rs:100-104,408,531-540; store/local.rs:295-300.

Start a host migration over HTTPS, then forget the alias before the successful response returns. Forget deletes local operations/publications and removes the connection from Core, but shutdown cancels only an existing bridge client, not the HTTP migration or adoption work. finish_op treats a missing operation as if it were still pending. apply_manifest inserts the publications and finishes an operation with an UPDATE that succeeds despite affecting zero rows. Both original migrate and resume_migration then start the retained connection Arc.

The parent probe calls actual publish::forget, asserts the alias/op are gone, then supplies the late successful manifest through finish_op. It fails with Some(Published) versus expected None. Local publication recreation is runtime-proven; subsequent reconnection follows the actual migrate/resume callers in source, rather than a network assertion in this unit probe. The user has forgotten the board, yet late work can restore its inbound publication state.

Fix: make forget cancel outstanding adoption/migration, prevent start on a removed connection, and require the operation still to be pending within the manifest transaction. A removed operation must not authorize local side effects.

Probe: /home/kdobmayer/.cache/conde-review/design/v2-674d78a-security-late-migrate-probe.rs (append inside existing daemon::board::tests).

### 2. P2 — Response arrival order still cannot establish mailbox snapshot freshness

Locations: crates/conde/src/daemon/board.rs:240-248,309-320; crates/conde-remote/src/server.rs:515-523,1099-1105; client.rs:813.

A mailbox.mine read captures no mailbox. A concurrent publish then commits and its response creates the local Published row. The earlier mine response arrives afterward; its larger reception sequence passes the completion-marker comparison and deletes the fresh local publication. Requests are independently spawned by the real server, so transaction/snapshot order is not response-enqueue or reception order. The callback-order guard fixes delayed handling of already-ordered frames, but not this ordering of actual server responses.

Parent reproduced this using the real cloud Board, SQLite store, remote server/client and daemon. A wrapper pauses only the return of the already-completed mine read; publication proceeds through the real server. Releasing the stale empty response deletes the row: None versus expected Published. No fabricated result or fake publication state is used. This is the remaining form of first-pass finding2. Route frames do not recreate the deleted local-agent mapping, and unchanged snapshots need not be emitted again.

Fix: use snapshot/commit revisions with meaningful server ordering, or conservatively protect publications completed since the mine request began and obtain a fresh snapshot. A frame reception counter alone cannot establish causality.

Probe: /home/kdobmayer/.cache/conde-review/v2-674d78a/crates/conde/tests/mine_order_review.rs, review_older_mine_snapshot_arriving_after_publish_must_not_delete_it.

### 3. P3 — sent omits shared-board pagination hints

Locations: crates/conde/src/surface.rs:381-400; cli.rs:629; mcp.rs:578; ops/messages.rs:370-391; daemon/mod.rs:695-696; README.md:484-488.

The backend returns a continuation after PAGE=200 receipts and aggregation preserves it in boards[].next_cursor. surface::sent never invokes the continuation formatter, so CLI and MCP show only the first page with no recovery hint. The local omission predates V2; discarding the new shared aggregate cursor is in this range. The parent formatter probe fails with only the receipt line rendered, despite a shared cursor of200.

Fix: pass Surface to sent and use more(..., "sent") for CLI/MCP; add continuation assertions. Probe: /home/kdobmayer/.cache/conde-review/v2-674d78a/crates/conde/tests/surface_review.rs.

## Closed during this pass

- First-pass1 duplicate row transitions: fixed. Separately, 674d78a still advanced completion marks on duplicate replies before the duplicate guard; parent reproduced retained Published after a valid withdrawal. Follow-up2dfae90 moves marking into the transaction after duplicate/ownership checks and only on actual Publishing→Published. Three focused board unit tests pass at exact2dfae90, including the copied negative probe.
- First-pass3 local terminal receipt loss: fixed; included test passes, and parent reran the exact prior Held→Delivered abort-trigger probe, now passing retention, recovery and release.
- First-pass4 shared read selection and surface: captured IDs are read individually; an intervening DM remains unread; unconfirmed previews carry a qualified whole-message recovery command. CLI/MCP regressions pass.
- First-pass5 missing default fallback: unknown defaults refuse; forgetting the configured default resets explicitly and reports it. Doctor origin removal passes.
- First-pass6 moved-generation outcomes remains closed on both stores.
- First-pass7 and follow-up history integrity findings remain closed; included history12/12 pass again at this integrated snapshot. History implementation is unchanged since accepted6bea34c.
- First-pass8 shared resolve/profile and9 inbox continuation are fixed. Finding3 above concerns sent, not the fixed inbox path.
- G6 now reaches the exact pre-send crash point with zero captured outcomes. G11b now exercises two independent shared boards plus local delivery, including capacity beyond R and a retained late receipt. The longer eventual-state waits preserve explicit latency bounds.
- Infra0e94257 wires CONDE_BOARD_NAME and CONDE_MIN_PROTO to the matching server settings; default protocol1 preserves staged upgrade. No live infra applied or verified.

## Verification

Exact674d78a: 201 included tests passed with no skips in the selected suites: conde lib56, cli_wiring12, cloud10, history12, mcp7, recovery11, shared_board29, v2_daemon29; conde-remote protocol_v2 10 and remote25. PostgreSQL18.3 ran in a fresh disposable cluster, so PG-marked paths executed. The exact prior local Held→Delivered failure probe additionally passed.

Four additional negative probes fail at674d78a: duplicate completion marker, delayed real mine response, late migration after forget, and sent continuation. The marker finding was fixed during review at2dfae90 and its three focused unit tests pass there. Parent also reran the real delayed-mine probe at exact2dfae90: it still fails with None versus Published, confirming the remaining snapshot-order finding is independent of that fix. No full-workspace or clippy rerun by the parent; peer reports of those checks are separate evidence.

All this second-pass runtime fixtures/probes were synthetic. No production credentials, private purge/history source, live central content, service changes, deployment, or switch recipe execution. Earlier private local import-fixture reads remain disclosed in prior reports and were not repeated here. Shared repository untouched; probes live in isolated cache snapshots.

## Rejected candidates and practical limits

Same-process modern re-enrollment replacing a forgotten connection was refuted: enrollment writes config and requires a daemon restart to load a missing alias. Finding1 needs no replacement alias; existing pending migration owns the old Arc. Unknown-default local rerouting, broad reads consuming newly arrived DMs, local identity forwarding, and history-copy endpoint reintroduction were refuted in current source.

Runtime tests and source review do not establish production rollout success. The two remaining P2 reproductions are what make this range unacceptable for rollout despite the passing included suites.
