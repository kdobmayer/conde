# Integrated v2 review — c385bcd..c6fc4b0

**Request changes.** Six Sol lenses completed (server security, surface/routing claims, v1/database regression, tests, delivery, history), followed by parent source verification and isolated probes. The following findings are deduplicated. All references below are to c6fc4b0; later follow-ups need their own disposition.

## Findings

1. **P2 — Repeated local operation completion corrupts a newer publication transition.** `daemon/board.rs:345–388`. The originating request and the reconnect pending-op worker (`:197–206`) can both send/finish the same stored operation. Remote idempotency does not make `finish_op` locally idempotent: it never rereads the operation state before mutation. An old publish completion changes a newer Unpublishing row back to Published, reopening local admission; an old unpublish completion deletes a subsequent Publishing row. **Two isolated unit probes failed with exactly these outcomes.** Recheck operation state and transition ownership inside the local transaction; completed duplicates must return their stored result without touching newer state.

2. **P2 — A queued older routes snapshot can erase a newly completed publication.** `daemon/board.rs:166,275–281`. Routes callbacks reconcile with no snapshot guard, while RPC completion independently marks a publication Published. An empty routes snapshot can be received/queued before the publish, but handled after its completion; reconciliation then deletes the new publication. A later full snapshot cannot recreate the local-agent mapping. The client handles responses separately from its notification callback queue, so wire ordering alone does not serialize these effects. Order snapshots and mutation completion through a common sequencing mechanism or carry a server revision that identifies stale snapshots. Source-confirmed; add a callback-delay regression rather than assuming immediate event processing.

3. **P2 — Local delivery loses terminal receipts when the local report transaction fails.** `daemon/mux.rs:456–464,608–612,659–663`. `local_report` logs errors without returning success, and the caller releases the reservation/token regardless. A transient SQLite error while recording Delivered consumes the only receipt and forgets its mapping. **A synthetic SQLite abort-trigger probe failed:** the local reservation fell to zero before its terminal outcome was committed. Retain/retry local outcomes and release only after recording succeeds, including immediate delivery results.

4. **P2 — Aggregate read can consume unseen messages and silently display incomplete content.** `daemon/mod.rs:594–613`; `ops/messages.rs:272–277`; `surface.rs:249–276`. The daemon snapshots inbox, then sends an unrestricted read that recomputes the unread set. A DM arriving between the calls can be committed as read but absent from the fallback after the response is lost. Even a stable long message is replaced by its 80-character preview, yet CLI/MCP ignore `unconfirmed` and print it as a complete read without recovery instructions. **Both desired-invariant probes failed**, using the existing fake-board/real-daemon harness with a deterministic arrival before read dispatch and a committed read whose response is withheld. Fix both: bind the mutation to the captured message set (or durably recover the exact read response), and visibly mark incomplete/unconfirmed rows with qualified-ID recovery commands. This suppresses unread/content visibility; it does not physically delete message bodies.

5. **P2 — Forgetting the configured default board silently redirects messages locally.** `daemon/resolve.rs:133–135`; `daemon/publish.rs:240–242`. Forget removes the board but leaves `default-board`; the resolver maps an unknown default to Local. **Two probes failed**, including the shipped forget operation followed by a bare send: the message appeared in the local reviewer's inbox. Refuse an unavailable selected default, or explicitly and atomically reset it during forget and report that change. Never silently infer another destination.

6. **P2 — A delayed outcome can settle a notification after its mailbox moves.** `ops/bridge.rs:302–306,333–345`. Host/instance ownership is checked, but current recipient binding generation is not. A takes an attempt, B publishes with `--take`, then A's delayed Delivered arrives before the next claim performs stale-attempt cleanup: the notification is completed instead of remaining pending for B. Check route generation/current binding inside the report transaction and abandon stale attempts. Source-confirmed by both security and regression lenses. **Desk already has a follow-up for this; it remains open for this exact head until that commit is reviewed.**

7. **P2 — Inventory accepts an incomplete snapshot and misclassifies imported history.** `history.rs:891–915`. Snapshot parse errors do not prevent manifest generation and are omitted from the exit status. **The synthetic probe failed:** corrupting both copies of one imported DM produced two parse errors, zero snapshot messages, a class-(b) row instead of class (a), no missing/hash-difference errors, and exit 0. Refuse to produce an accepted freeze/classification manifest unless snapshot parsing is complete and error-free.

8. **P2 — Shared resolve/profile formatters still require the deliberately removed address.** `cli.rs:672,686`; `ops/agents.rs:247–252,282`. Shared resolve returns name/kind/host without address; the CLI prints a blank line. Profile incorrectly labels the same registered mailbox `(not registered)`. Render the shared shape and retain address formatting for local rows. Producer/consumer source-confirmed.

9. **P3 — Aggregate inbox omits shared-board pagination hints.** `daemon/mod.rs:660–661`; `surface.rs:239–243`. The daemon puts shared continuation cursors under `boards[]`, but the formatter reads only top-level `next_cursor`. With 201 shared unread messages, only the first 200 appear without the command needed for the remainder. Render per-board `--board <alias> --after N` hints and equivalent MCP arguments. Source-confirmed.

## Required gate corrections

- G11b currently uses one shared board plus local. Two recipients on that board do not test isolation between two shared origins. Add a second shared board, exceed R there while the first stays paused, then settle the first board's late receipt. The current source budgets are per origin; this is a discriminating-test gap, not an observed shared-budget defect.
- G6's purported pre-send kill waits until the fake server has captured the outcome. It checks an uncommitted outcome, but does not establish the exact client-memory-before-send crash point. Correct the claim or add that controlled point.

## Validation and limits

Existing suites passed despite the findings:

- c6fc4b0: `v2_daemon` 23/23, including real integrated exchange/G11/G11b, first with SQLite and again with PostgreSQL configured for the real-board cases.
- `shared_board`: 13 actual SQLite cases on the first run (12 PostgreSQL wrappers returned early), then all 25 exercised with PostgreSQL configured, including both backends and the SQLite rebuild.
- `history`: 4 exercised plus one PostgreSQL skip initially; all 5 exercised with PostgreSQL configured.
- Rewritten cloud process suite: 10/10 on an isolated PostgreSQL 18.3 cluster, including real v1/v2 daemon traffic in both directions.
- Eight new negative probes failed as expected: duplicate completion x2, routing/read x4, inventory x1, local receipt persistence x1. These used synthetic data in the cache snapshot; no production files or private data were changed.

The panel also verified old-schema SQLite rebuild preservation of bindings/pins, message/reply graph dependencies, read state, reactions, held attempts and foreign keys. Legacy delivery/forwarding bodies remain equivalent to the baseline. No new demonstrated automatic harness-ID leak was found in the reviewed shipped v2 producers: outcomes use the closed enum, current directory reads are target-free, enrollment does not publish, and unpublished sends fail before forwarding. These strengths do not clear the publication-state and delivery failures above.

No full-workspace/clippy rerun, live board inspection, real private-history rehearsal, image verification, deployment or homepc switch was performed. The lane's broader reported gates remain separate evidence.

## History follow-up

The missing offline safety-copy restore at c6fc4b0 was a homepc-switch gate, not a server-image defect. Desk has supplied `1961e29` + `8ef4b50` for a separate review: offline CSV restore, after-freeze rows, pruning, and removal of `history.copy`. That follow-up is not cleared by this report. H1's parse-error finding remains applicable.

What would make me reject the rollout? Reopening revoked local admission through stale completion, losing receipt/read state under recoverable failures, silently choosing a different destination, or accepting an incomplete restore manifest. Those conditions are established above. Keep the roll/switch pending the corresponding fixes and follow-up verification.
