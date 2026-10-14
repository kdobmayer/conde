# Confirmation at 1be9973

Scope: c385bcd..1be9973 cumulatively, focused delta9fc15d8..1be9973 (board.rs and design). Four fresh independent Sol lenses completed. Exact archived source: /home/kdobmayer/.cache/conde-review/v2-1be9973.

The grouped revision-ordering P2 is CLOSED. No remaining P1/P2 identified in this confirmation. One nonblocking P3 remains in CLI output.

## P3: withdrawn publication is printed as pending

crates/conde/src/cli.rs745-753 handles only state=published explicitly. The new terminal withdrawn result falls into the pending branch, claiming the board did not answer and the daemon will complete the operation on reconnect. In fact board.rs495-499 deleted the publication and stored the operation Done; publish.rs142-144 forwards withdrawn, and duplicate completions correctly return withdrawn too. Render that terminal state explicitly and do not promise retry/reconnect completion.

Parent runtime reproduction: the real CLI binary receives the new withdrawn result through the existing synthetic daemon fixture. review_withdrawn_publish_is_terminal_in_cli fails, printing:

> publishing worker on dakai is pending (op 01DONE): dakai did not answer; the daemon completes it when it reconnects, and conde doctor shows it until then

Probe file: /home/kdobmayer/.cache/conde-review/v2-1be9973/crates/conde/tests/withdrawn_review.rs.

## Why the ordering blocker closes

Revision-bearing snapshots now decide by revision, without the frame-order fallback suppressing a newer withdrawal. Reconciliation saves the complete revised snapshot even when the row is still Publishing. An older publish answer settles against that snapshot: absent mailbox produces a terminal withdrawn operation and no publication; a present mailbox adopts the current generation. An unrelated mailbox advancing the host revision therefore does not wrongly withdraw a still-present target.

Parent ran6 included board unit tests +31 shared_board tests +30 v2_daemon tests =67 passed. PostgreSQL18.3 was enabled on a fresh disposable cluster; both-store routes/revisions and real-server integration paths executed, no skips. Both formerly failing causal-order probes pass unchanged.

Parent added two narrow positive probes in the isolated snapshot: (1) withdrawn completion and its duplicate both return withdrawn and leave no publication; (2) a newer snapshot still containing the mailbox preserves Published and advances generation2→3. Both pass (run with3 already-passing review tests). Artifact: /home/kdobmayer/.cache/conde-review/design/v2-1be9973-positive-snapshot-probes.rs.

## Limits

The prior272-test result belongs to9fc15d8, not a full rerun at1be9973. Store/schema/server wire are unchanged by this final delta. Parent did not rerun workspace/clippy; peer reports are distinct evidence. No private datasets, production credentials, live board traffic, deployment, service changes or switch recipe were used. Shared repository untouched. Runtime probes and all builds ran on cache archives with synthetic fixtures.

The CLI P3 is a genuine feedback defect but does not reopen the route-state P2. This is a code-review verdict, not a claim that rollout or live gates have succeeded.
