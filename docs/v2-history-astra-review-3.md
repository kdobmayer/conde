# History follow-up at 6bea34c

Scope: history/import/CLI/design in 1961e29..6bea34c; focused fixes 6c3fc32 + 6bea34c, reviewed against prior history-9f7caa7-astra-review.md. Exact archive: /home/kdobmayer/.cache/conde-review/history-6bea34c.

All four targeted findings are closed. No remaining correctness blocker found in this history scope.

- Partial final CSV records now fail before restoration; consumed IDs, numeric fields, timestamps and attachment sizes are checked before writes. The exact ten-byte truncation regression passes.
- Errors introduced while planning registry/pin bindings now make prune-candidates fail before output.
- DESIGN and generated manifest instructions no longer require --after-freeze and distinguish copied versus centrally retained rows.
- Class(a) accounting now explicitly splits rows present from H2 versus excluded by pruning.

One optional comment cleanup remains: crates/conde/src/history.rs:11-12 says later DMs are copied “on request”; saved newer DMs are now included unconditionally. This is not a switch-command or restore behavior defect.

## Verification

Four fresh independent Sol lenses completed: claims, regression, security, tests. Parent ran the 12 included history tests with a fresh disposable PostgreSQL18.3 cluster: 12 passed, no skips, including actual extraction and PostgreSQL COPY checks. Parent additionally ran a synthetic timestamp-only negative probe: a complete newline-terminated CSV record with valid IDs/counts but created_at=2026-10-09T10:1 is refused, reports created_at, and leaves all table counts unchanged. This isolates the timestamp guard from the EOF and invalid-ULID guards; the included complete-record case changes multiple fields.

Parent real CLI synthetic probes separately supplied a malformed registry address and a malformed pin: both exit1 and produce no prune TSV. Existing positive copy, retained verification, whole-row deletion including edited counts, malformed-source, prune, and copy replay cases pass.

No private datasets, credentials, live central reads or switch commands were used in this follow-up. Peer real-data rehearsal totals were not independently reproduced. No shared repository edits; extra probe exists only in the cache archive. Full workspace and V2/V3 integrated range were not rerun for this narrow follow-up.

## Rejected candidates / limits

- Adding a newline cannot bypass the independently checked timestamp format (parent probe).
- Genuine row counts plus the prior partial-record truncation no longer yield a successful copy.
- Pruning does not skip strict source-record validation; excluded class(a) rows are separately accounted for.
- Missing frozen rows still require explicit purged-party accounting; retained rows still require actual authorized central reads and body hashes.
- Timestamp checks validate serialized shape, not calendar ranges. Purge counts/preview remain trusted source metadata; retained verification proves body hashes rather than every metadata/attachment field. These limits do not reopen the reproduced findings.

Acceptance is limited to this history implementation review, not production execution, deployment, or the pending second integrated review.
