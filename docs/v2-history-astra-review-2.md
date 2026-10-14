# History follow-up review at 9f7caa7

Scope: history/import/CLI/design changes in 1961e29..9f7caa7, including ad2d562, b6bc6bb, 9f7caa7. Exact archived source under /home/kdobmayer/.cache/conde-review/history-9f7caa7. No implementation or live migration performed.

## Findings

1. **P2: a truncated final CSV record still passes completeness and commits corrupted newer-message metadata.** `crates/conde/src/history.rs:1039-1042,1234,1291,1443`. PostgreSQL COPY output cut inside the last unquoted created_at field retains the same row/column counts. csv_records accepts EOF as a complete record; read_saved trusts the shortened timestamp, and newer rows lack the frozen metadata comparison. Synthetic parent probe removed the final ten bytes of messages.csv, retaining authentic counts.txt and preview.txt and all four rows. The original timestamp `2026-10-09T10:19:59.549Z` became stored `2026-10-09T10:1`; copy committed three rows and printed `result: OK`. Require the complete record termination of the source COPY format and validate required saved fields before any insert (or verify independently recorded file integrity). Add the partial-record case beside the existing whole-row deletion case. Reproducer: ../history-9f7caa7/crates/conde/tests/history_review.rs:694, test `review_truncated_newer_metadata_must_be_refused`; it fails on this snapshot.

2. **P3: prune-candidates still accepts registry/pin parse errors added during planning.** `crates/conde/src/import.rs:1110`. complete_scan rejects scanner errors, but plan then parses addresses and can append errors; agent_activity never checks those, unlike snapshot at1230. Parent synthetic real CLI probe used registry/lead containing `not-an-address` and an empty board directory: exit0 and a prune TSV were produced. Check post-plan errors before emitting activity. This does not reopen the lost-message inventory blocker: snapshot rejects these errors and H2 still reports issues.

3. **P3: instructions still use the removed --after-freeze option.** `docs/DESIGN-v2.md:705` and generated manifest prose at `crates/conde/src/history.rs:791-792` show the obsolete flag; the current CLI rejects it. Generated text at802 also describes copying exactly all class(b), though some are retained and verified centrally. Update both producers to the new unconditional-newer and restored/retained accounting. The dedicated SWITCH-v2-history.md recipe already uses valid syntax.

4. **P3: class(a) rows excluded by pruning are described as restored by import.** `crates/conde/src/history.rs:1427-1433` increments saved_imported and continues before skipped accounting. Display labels that whole count restored by the import, although the later local check intentionally requires pruned rows to be absent. Distinguish class(a) imported versus pruned in the summary, or label the count as classification only. Data exclusion itself is correct; this is report accuracy.

## Closed and rejected candidates

- Original finding7 lost-message parse-error path is closed: snapshot refuses scanner and post-plan errors before manifest output. New CLI regression verifies no output files.
- Original missing frozen DM / presumed-still-central P2 is closed: purged-party partition identifies missing saved rows even with edited counts, and retained rows require actual authorized message.get plus body hash before local writes.
- Whole-row omission of newer data with jointly edited counts is not an additional finding: that changes the independent export metadata too. Genuine unchanged counts catch whole-row omission. Finding1 needs no counts change.
- Wrong-account retained verification cannot bypass message.get ownership/party authorization. Redirects are disabled; credentials and message bodies are not printed.
- Retained verification proves body hashes, not every attachment/metadata field; source-count checks assume genuine purge metadata.

## Verification and limits

Parent ran all 9 history tests on the exact archive with a fresh disposable PostgreSQL18.3 cluster: 9 passed, including both actual PostgreSQL tests, no skips. Parent partial-record negative probe failed twice (second run additionally read back the damaged timestamp). Parent malformed-registry CLI probe confirmed exit0/output. Four independent Sol lenses reviewed claims, regression, security and tests; their tests were source-inspected, parent owns execution.

All this follow-up's fixtures and probes were synthetic. No production purge CSV, private history bodies, credentials, live central verification or switch commands were accessed/executed in this follow-up. Earlier import-suite private local-source reads remain disclosed in the prior report; this statement applies only to this follow-up. The supplied real rehearsal counts are peer evidence, not independently reproduced here. No full-workspace rerun. Pending V2/V3 changes are outside this snapshot.

Verdict: request changes for finding1 before accepting this history gate; originals are closed with the lower-severity residual items above.
