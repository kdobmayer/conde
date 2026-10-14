# Offline history follow-up — 1961e29 and 8ef4b50

**Request changes before the homepc switch.** The offline source implementation closes the earlier missing-restore-path gate, but completeness checking still accepts missing saved history. The existing H1 parse-error finding also remains open.

## P2 — Missing saved messages are falsely classified as remaining on central

`crates/conde/src/history.rs:1233` computes `on_central` as all frozen class-(b) IDs minus those seen in the CSV. Absence from a saved file is not evidence that a message remains on central. A truncated/incomplete saved messages file can therefore omit a purged message, yet pass hash validation of the subset and write a successful partial restore.

**Confirmed by an isolated synthetic probe:** remove one known-saved frozen DM without attachments/dependent replies from the test CSV, then run the actual copy path. It wrote the remaining rows in one transaction, increased the alleged central count from one to two, and printed `result: OK`. The desired-invariant assertion failed. Nothing in the supplied data proved that omitted message still existed centrally.

Require an independently frozen inventory of saved/purged IDs (and source-file integrity), plus an explicit reviewed retained-on-central set. Verify the complete required set before writes, after applying the approved prune list. Do not infer retention from missing CSV rows. Include post-freeze rows in the source completeness check as well as the frozen subset.

Probe: `/home/kdobmayer/.cache/conde-review/history-8ef4b50/crates/conde/tests/history_followup_review.rs`, test `review_missing_saved_frozen_dm_must_not_be_reported_as_still_on_central`.

## Existing P2 — Incomplete H1 snapshot still accepted

`history.rs:870–894` retains the previously reproduced behavior: parse errors do not prevent manifest generation or successful exit. Fix remains required; see finding 7 of the integrated core report. This was not counted as a new finding in this follow-up.

## What closed and what was rejected

- H3 now reads the local saved CSVs; the network `history.copy` operation is removed. The previous dependence on already-purged server rows is closed.
- Included frozen message bodies and attachments are checked against H1 before insertion; IDs, replies, read state and reactions are restored transactionally. This proves equality of included rows, not completeness of the source.
- The new instructions put prune-candidate review before import/restore and use a fresh kond snapshot for H2. Post-freeze restoration is explicit.
- Rejected a suspected skipped-reactor regression: on a valid saved DM, current API reactions belong to one of its parties; pruning that party already removes the DM. A third-party reactor requires invalid source relationships, so no ordinary-data regression was filed.

## Independent evidence and boundaries

- New history suite: all 6 cases exercised and passed; the 2 PostgreSQL-dependent cases were rerun on an isolated PostgreSQL 18.3 cluster after the initial SQLite run.
- Import suite: 24 exercised cases passed; the PostgreSQL-only import case returned early because `CONDE_TEST_PG` was unset. The container-style command test exercised its SQLite branch.
- New missing-saved-row probe failed as expected, producing the partial successful restore described above.
- Validation correction: the import suite automatically discovered the owner's local kond board and frozen snapshot. Its existing real-board-copy dry-run and frozen-snapshot import tests ran locally against temporary copies/stores. No source data was modified or uploaded and no message bodies were printed in tool output. These checks were not a rehearsal of the new safety-copy CSV restoration. I did not inspect the actual purge CSV contents or execute the switch recipe.

The implementation lane's reported real-data 49-row rehearsal remains its evidence, not independently reproduced here. No shared-board operation, service stop, prune, production import or switch was executed by this review.

What would make me reject the switch? A successful report that cannot distinguish an intentionally retained message from an accidentally missing saved message. That failure is reproduced at 8ef4b50.
