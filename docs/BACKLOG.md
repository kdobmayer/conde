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
  the release build; `conde setup` (rewrites skills and MCP entries that the
  release changed, reports unchanged otherwise); restart the bridge, `conde
  doctor` all clear (the bridge runs the new build), `conde setup --check` all
  clear; the bridge reconnects
  to dakai with the existing host credential (no re-login); one DM each way
  with desk. The interactive login (`conde login --server ... --board <alias>
  --code`) is exercised only when a release changes the login path and a human
  is around; otherwise the report says "login not exercised". One line when
  green; full detail when not.

## Queue, top first

- B7. [I] A sign-in `next` path over 2048 characters is dropped silently and
  a very long authorize request lands on /keys. Refuse it with a clear 400 (or
  carry it) and test the boundary. Area: crates/conde-remote web login.
- B11. [J] `conde admin key create` adds a key without revoking the host's
  live ones (the only overlap path; every other mint revokes and inserts in
  one transaction). Decided: revoke by default like the other mints, with
  `--keep-live` for a hand-over rotation; the command prints which it did.
  Area: crates/conde admin CLI + ops::hosts, tests.
- B13. Intermittent tests: setup_check_asks_the_live_daemon (2 s daemon-start
  gate), v2_daemon g1 (700 ms register gate) and Postgres shared_board
  a_read_hold_is_kept_when_the_mailbox_leaves_the_endpoints ("held" vs "read",
  ~line 1092, not a timing gate: find the ordering it depends on) each failed
  once on a loaded box and passed on rerun. Make each gate measure what it
  claims (a bound relative to a baseline measured in the same run, or a
  generous absolute bound with the tight one behind an env flag for the
  release machine). Area: tests only.
- B14. Test harness port race: web_ui
  history_pages_back_in_chronological_order failed once at web_ui.rs:146
  (server start unwrap after the harness freed a probe port) and passed on 5
  reruns; make the harness hand the server a bound listener instead of a freed
  port number. Area: tests only.
- B15. Upgrade path says to run `conde setup`: after an upgrade-install the
  installed skills can differ from what the new release writes (the NUC smoke
  on 01eff10 saw setup --check report both skills as differing until setup ran).
  Make install.sh print "run conde setup" when a conde was already installed,
  add an Upgrade section to the README, and consider setup --check printing the
  one command that fixes it. Area: install.sh, README, crates/conde setup.

## Waiting on a decision or an observation (do not take)

- W1. [I] Refresh reuse grace (a token consumed less than 10 s ago answers
  invalid_grant without revoking) and client-metadata pinning per host: both
  wait for the owner's first remote MCP connect and its log.
- W2. [E] Windows acceptance: the unverified list in NEXT.md section E waits
  for a run on a real Windows machine with desk watching.
- W3. [D] Web UI images: the brief is in NEXT.md section D; generation goes
  through the owner's design partner, not a contributor lane.
- W4. Web UI overhaul (owner, 2026-10-14: the current layout and visuals are
  not good enough; the new UI must be well thought out, visually striking,
  well organized) with Konstantin's live Feed folded in (every message across
  the owner's agents, thread view, filters, live push, owner-scoped,
  read-only). Design phase at desk first (audit, information architecture,
  three rendered directions, plan); implementation items appear here after
  the owner picks a direction.
