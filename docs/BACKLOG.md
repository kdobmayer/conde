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

- S1. Release smoke on the NUC after every release, unattended with the
  existing enrollment: upgrade-install from
  https://conde.nativeai.space/install.sh (checksum), `conde --version` shows
  the release build; restart the bridge, `conde doctor` all clear (the bridge
  runs the new build), `conde setup --check` all clear; the bridge reconnects
  to dakai with the existing host credential (no re-login); one DM each way
  with desk. The interactive login (`conde login --server ... --board <alias>
  --code`) is exercised only when a release changes the login path and a human
  is around; otherwise the report says "login not exercised". One line when
  green; full detail when not.

## Queue, top first

- B5. [J] Redeem answer: `reenrolled` means the host existed, not that a live
  key was revoked; the mint knows `replaced`. Return both and make the CLI say
  which ("re-enrolled host, previous key revoked" vs "re-enrolled host, no live
  key before"). Area: crates/conde-remote redeem + crates/conde login CLI,
  tests.
- B7. [I] A sign-in `next` path over 2048 characters is dropped silently and
  a very long authorize request lands on /keys. Refuse it with a clear 400 (or
  carry it) and test the boundary. Area: crates/conde-remote web login.
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
  build with the binary's, like doctor does (the NUC smoke saw "all clear"
  from setup --check with the old bridge still running); setup restarts the
  service on a mismatch, --check reports it as an issue. Area: crates/conde
  setup, tests.
- B13. Timing gates that fail under load: setup_check_asks_the_live_daemon
  (2 s daemon-start gate) and v2_daemon g1 (700 ms register gate) each failed
  once on a loaded box and passed on rerun. Make each gate measure what it
  claims (a bound relative to a baseline measured in the same run, or a
  generous absolute bound with the tight one behind an env flag for the
  release machine). Area: tests only.

## Waiting on a decision or an observation (do not take)

- W1. [I] Refresh reuse grace (a token consumed less than 10 s ago answers
  invalid_grant without revoking) and client-metadata pinning per host: both
  wait for the owner's first remote MCP connect and its log.
- W2. [E] Windows acceptance: the unverified list in NEXT.md section E waits
  for a run on a real Windows machine with desk watching.
- W3. [D] Web UI images: the brief is in NEXT.md section D; generation goes
  through the owner's design partner, not a contributor lane.
