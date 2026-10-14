# Confirmation review at 9fc15d8

Requested range c385bcd..9fc15d8; focused delta after reviewed2dfae90: 52d3289,8e885d1,9fc15d8. Five fresh independent Sol lenses completed: regression, security, claims, tests and stores. Exact snapshot /home/kdobmayer/.cache/conde-review/v2-9fc15d8.

Verdict: REQUEST CHANGES. One grouped P2 remains; two parent-executed negative probes fail. Prior forget-publication and sent-pagination findings close.

## P2: revision ordering does not yet fence older publication answers

Locations: crates/conde/src/daemon/board.rs:325-334 and477-487. A publish commits at host revision5, but its response is delayed. Another host takes that mailbox, producing an empty revision6 routes snapshot. That snapshot arrives as frame8 before the older publish response at frame9. Both callback orders remain wrong:

1. Publish response handled first: local Publishing becomes Published with completion-frame9. Reconcile accepts revision6 as newer, but its per-row frame guard skips removal because9>8. A newer authoritative revision is subordinated to arrival order.
2. Routes callback handled first: reconcile records revision6 but skips the still-Publishing row. The later revision5 publish answer changes the row to Published and keeps max revision6, losing the already-consumed withdrawal snapshot.

Parent appended the supplied two probes inside existing board tests and ran both at exact9fc15d8. Both fail with Some(Published) instead of None. Probe artifact: /home/kdobmayer/.cache/conde-review/design/v2-9fc15d8-causal-order-probes.rs.

This sequence has a shipped producer: mailbox takeover in ops/mailbox.rs48-75; server polling alongside independently spawned request handlers at conde-remote/src/server.rs895-896,1099-1105; client callbacks on a separate thread at client.rs347-355. The tests control handler ordering using valid snapshots/responses and real local-store transactions; this turn did not add a second network gate for this converse ordering. The prior real-server delayed-read proof established the same independent response/callback scheduling architecture.

Impact: the former host retains an apparently active local publication after takeover. Repeating publish returns unchanged locally without asking the board (publish.rs110-111,137-139); unchanged route snapshots are deduplicated (server.rs1000-1003), so repair is not guaranteed until another route event or reconnect.

Fix: authoritative causal revisions must outrank the frame-order fallback. When a publish reply predates an already-consumed snapshot, retain/reapply that snapshot or schedule fresh reconciliation; a maximum revision counter without its snapshot is insufficient. Do not simply discard every older publish answer: a newer host revision may reflect an unrelated mailbox change, so reconcile the actual mailbox against current authoritative state.

## Closed / verified

- Original delayed empty mailbox.mine response no longer deletes a newer publication: lower revision is rejected, and the copied real-server regression passes.
- Duplicate completion marker remains fixed: duplicate operation guards precede revision/marker changes.
- Late migration completion after completed forget no longer recreates publications. The guard is checked inside the apply transaction; missing operation completion must affect exactly one row or roll back. Included original probe passes.
- Sent continuation now renders correct CLI and MCP hints, both aggregate and explicit-board paths. Dedicated tests pass.
- Store revisions for normal runtime set_binding changes are updated atomically on SQLite and PostgreSQL, including both old/new hosts on takeover; routes and revision are read in the operation transaction. Migration0007 applies cleanly.

## Validation

272 included tests passed, no skips: conde library59, cli_wiring13, cloud10, history12, mcp8, protocol_v2 10, recovery11, shared_board31, store53, v2_daemon30; conde-remote protocol_v2 10 and remote25. Actual PostgreSQL18.3 ran on fresh disposable clusters, including both-store revision checks and the53-case store/migration suite. Two additional causal-order probes fail as described above.

No unfiltered import suite, private data, production credentials, live board traffic, infrastructure mutation or rollout was used. Sources and probes are in isolated cache archives; shared repository untouched. Parent did not rerun full workspace/clippy; peer reports of those gates are distinct evidence.

## Rejected candidates / limits

The store lane initially identified direct bound-agent import as bypassing revision increments, then withdrew it as a blocker: live incremental import exceeds the documented one-off pre-cutover workflow (cli.rs227-229, infra/README.md52, DESIGN-v2.md716-725). Supported pre-v2 imports have no existing publication mappings; host.migrate_v2 establishes revisions through set_binding. Runtime refusal would be optional hardening.

The forget data-loss/exposure reproduction is closed. Source review noted a late board.info adoption may recreate an empty migration-op record, while the forgotten flag blocks connection start; no renewed publications/admission were established. A check-versus-install start/forget concurrency window was noted but not independently reproduced; it is not promoted to an additional finding or a private-data disclosure claim.

These limits do not replace the confirmed ordering blocker. The two failing causal-order probes are what prevent acceptance of9fc15d8.
