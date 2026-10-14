# conde backlog

The standing queue for contributors. Pull from the top; claim an item with one
line on your thread with desk ("taking B3") before starting, so no two lanes
take the same item. Each item names its area (files that change) so two lanes
can run in parallel when their areas do not overlap. Details and history live
in docs/NEXT.md (section letters in brackets). Rules for every item: branch
from the fresh main, one PR, one commit when the parts share files, one-line
commit messages, no attribution, `cargo test --workspace` green on both stores
(CONDE_TEST_PG), no new clippy warning, docs updated where behavior changes.
Review and integration by desk, one batch at a time; one release per batch.

## Standing items (no claim needed)

- S1. Release smoke on the NUC after every release: fresh install from
  https://conde.nativeai.space/install.sh, `conde --version` shows the new
  build id, `conde login --server https://conde.nativeai.space --board <alias> --code`,
  `conde setup --check`, one DM each way with
  desk. One line on the thread when green; full detail when not.

## Queue, top first

- B2. [H] Rate limit on /auth/cli/redeem per IP (and per account), like the
  OAuth endpoints: today a client that re-issues (the same account confirming
  again) resets the 5-try budget. Shares the limiter module. Area:
  crates/conde-remote web ratelimit + redeem handler, tests.
- B3. [H] Pasted-code confirm by another account on an expired login answers
  403 before the expiry check; it should say "expired, run conde login again".
  Plus: `conde login --code` without a terminal exits 0 after printing the URL,
  so a script cannot tell "waiting for the code" from "enrolled": a distinct
  exit code or a machine-readable line. Area: login handler + CLI login
  command, tests. Small; pairs with B1 in one PR if the same lane takes both.
- B5. [J] Redeem answer: `reenrolled` means the host existed, not that a live
  key was revoked; the mint knows `replaced`. Return both and make the CLI say
  which ("re-enrolled host, previous key revoked" vs "re-enrolled host, no live
  key before"). Area: crates/conde-remote redeem + crates/conde login CLI,
  tests.
- B6. [I] `search` with a never-matching pattern scans all visible history,
  bounded only by the 120/min bucket. Bound the scan (a row budget per request
  with a cursor to continue, or a time budget) on both stores, CLI and MCP.
  Area: store search (both backends), dispatcher, CLI/MCP search, tests.
- B7. [I] A sign-in `next` path over 2048 characters is dropped silently and
  a very long authorize request lands on /keys. Refuse it with a clear 400 (or
  carry it) and test the boundary. Area: crates/conde-remote web login.
- B8. Clippy: the 4 pre-existing `result_large_err` warnings in unchanged code
  (`cargo clippy --workspace --all-targets`). Box the large error or restructure;
  no behavior change. Area: named by the warnings.
- B9. [J] Local build id names HEAD and the date of the first build of that
  commit; a later rebuild keeps the older date and uncommitted changes are not
  marked. Make build.rs rerun on tree changes and append `-dirty`. Area:
  build.rs, release.yml build-id job unchanged.
- B10. Test coverage push: pick the three least-covered modules of
  crates/conde and crates/conde-remote (measure with cargo-llvm-cov in your
  checkout; do not add it to the repo) and add tests that assert real claims,
  not line coverage. One PR per module. Area: tests only.
- B11. [J] `conde admin key create` adds a key without revoking the host's
  live ones (the only overlap path; every other mint revokes and inserts in
  one transaction). Decided: revoke by default like the other mints, with
  `--keep-live` for a hand-over rotation; the command prints which it did.
  Area: crates/conde admin CLI + ops::hosts, tests.
- B12. `conde setup` and `conde setup --check` compare the running bridge's
  build with the binary's, like doctor after B4 (the NUC smoke saw "all clear"
  from setup --check with the old bridge still running); setup restarts the
  service on a mismatch, --check reports it as an issue. Area: crates/conde
  setup, tests. Depends on B4.

## Waiting on a decision or an observation (do not take)

- W1. [I] Refresh reuse grace (a token consumed less than 10 s ago answers
  invalid_grant without revoking) and client-metadata pinning per host: both
  wait for the owner's first remote MCP connect and its log.
- W2. [E] Windows acceptance: the unverified list in NEXT.md section E waits
  for a run on a real Windows machine with desk watching.
- W3. [D] Web UI images: the brief is in NEXT.md section D; generation goes
  through the owner's design partner, not a contributor lane.
