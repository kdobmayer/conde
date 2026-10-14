# conde backlog

The standing queue for contributors. Pull from the top; claim an item with one
line on your thread with desk ("taking B3") before starting, so no two lanes
take the same item. Each item names its area (files that change) so two lanes
can run in parallel when their areas do not overlap. Details and history live
in docs/NEXT.md (section letters in brackets). Rules for every item: branch
from the fresh main, one PR, one commit when the parts share files, one-line
commit messages, no attribution, `cargo test --workspace` green on both stores
(CONDE_TEST_PG; on the release machine also CONDE_STRICT_TIMING=1 for the tight
timing bounds), no new clippy warning, docs updated where behavior changes.
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
- B15. Upgrade path says to run `conde setup`: after an upgrade-install the
  installed skills can differ from what the new release writes (the NUC smoke
  on 01eff10 saw setup --check report both skills as differing until setup ran).
  Make install.sh print "run conde setup" when a conde was already installed,
  add an Upgrade section to the README, and consider setup --check printing the
  one command that fixes it. Area: install.sh, README, crates/conde setup.
- B17. Late-outcome race family, remaining cases (from the PR 18 review): (a)
  mailbox gone with the message unread: a held outcome that commits after the
  endpoints frame that removed the mailbox stays held until the next endpoints
  frame or reconnect (gone_holds ran while the attempt was dispatching); (b)
  outcome lands just before the read: the notification stays held for a read
  message, counts in agent_health held and is re-offered through open_attempts
  on reconnect until the endpoint leaves. Decide the rule for each and test.
  Area: crates/conde ops/bridge.rs, store queries, tests/recovery.rs.

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
