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

- B11. [J] `conde admin key create` adds a key without revoking the host's
  live ones (the only overlap path; every other mint revokes and inserts in
  one transaction). Decided: revoke by default like the other mints, with
  `--keep-live` for a hand-over rotation; the command prints which it did.
  Area: crates/conde admin CLI + ops::hosts, tests.
- B20. install.sh staging dir: the new binary is staged under the default temp
  directory and moved over the target, so the final mv is a copy when temp and
  the target are on different filesystems (not atomic); stage next to the target
  instead. Area: dist/install.sh, dist_files test.

- B25. `conde watch` follow rows freeze their state: a row printed while
  following shows the state it had at arrival (sending, notified) and never
  changes, although the initial last-N rows show the settled state (read).
  Reported by Konstantin 2026-10-15. Decided (desk): a row printed while
  following leaves the state cell empty (the state is not known yet; the
  initial load and `--wide` keep it); README's watch paragraph says so. No
  re-reading of printed rows, no delayed rows. Area: crates/conde/src/surface.rs
  watch_row, the watch loop in cli.rs, README, tests/cli_wiring.rs.

## Waiting on a decision or an observation (do not take)

- W1. [I] Refresh reuse grace (a token consumed less than 10 s ago answers
  invalid_grant without revoking) and client-metadata pinning per host: both
  wait for the owner's first remote MCP connect and its log.
- W3. [D] Web UI images: the brief is in NEXT.md section D; generation goes
  through the owner's design partner, not a contributor lane.
- W4. Web UI overhaul (owner, 2026-10-14: the current layout and visuals are
  not good enough; the new UI must be well thought out, visually striking,
  well organized) with Konstantin's live Feed folded in (every message across
  the owner's agents, thread view, filters, live push, owner-scoped,
  read-only). Design phase at desk first (audit, information architecture,
  three rendered directions, plan); implementation items appear here after
  the owner picks a direction.

## Web UI v2 (owner 2026-10-14: skins A Console and B Editorial over one skeleton, per-browser switch, dark only; C not built; pick or mix after real traffic)

Design: docs/web-ui-v2/ (audit.md, ia.md, options/, plan.md). Each item below
is specified in plan.md with its gate; the lines here are the index. Server
items (S) run one at a time; client groups C1, C2, C3 are disjoint files and
can run in parallel with S; U6-U15 go to the branch web-v2, U16 merges it.

- U6. App shell, routing and the skin switch (default A). Area: web/src/routes/+layout.svelte, routes/+page.svelte, routes/{t,agents,a,channels,c,fleet}/**, lib/skins/**, lib/nav.svelte.ts, lib/UserMenu.svelte, app.css, app.html, test/skin.test.mjs. C1, after U1.
- U7. Data layer with the window rule (floor and exhausted, empty-after-pass restart, 10-page bound, 2 s cadence). Area: web/src/lib/{feed-window.ts,feed.svelte.ts,thread-view.svelte.ts,thread-tree.ts,types.ts}, scripts/design/mock-server.mjs, test/{feed,thread}.test.mjs. C1.
- U8. Filter menu, sheet, disclosure with interaction checks; state components. Area: web/src/lib/ui/**, routes/_design/primitives/+page.svelte, scripts/design/interact.mjs, test/ui.test.mjs. C1.
- U9. Feed and thread pages, plus the skin-switch preservation check. Area: web/src/routes/+page.svelte, routes/t/[id]/+page.svelte, lib/Thread.svelte, lib/FeedList.svelte, scripts/design/{shoot,interact}.mjs. C2, after C1.
- U10. Conversation, Agents and Channels pages. Area: web/src/lib/{Conversation,Delivery}.svelte, routes/{a,agents,c,channels}/**, test/conversation.test.mjs. C2.
- U11. Fleet page. Area: web/src/routes/fleet/**. C2.
- U12. Skin A, Console. Area: web/src/lib/skins/a/**. C3, after C2.
- U13. Skin B, Editorial. Area: web/src/lib/skins/b/**. C3.
- U14. Keys page restyle (outside the switch). Area: crates/conde-remote/src/web/pages.rs, web/scripts/design/mock-server.mjs. C3.
- U15. Fonts for skin B (latin + latin-ext woff2, SIL OFL). Area: web/static/app/fonts/**, web/src/lib/skins/b/fonts.css, web/src/lib/design/fixture.ts, web/README.md. After U13.
- U16. Remove the design routes; gate web-v2 on the real server (smoke on both stores); merge web-v2 to main. Area: web/src/routes/_design/**, routes/+layout.svelte, lib/design/model.ts, scripts/design/interact.mjs, crates/conde/tests/web_ui.rs, docs/web-ui-v2/options/README.md. Last, after U1-U5 on main and U6-U15 on web-v2.
- W-UI. The owner's A/B pick (or mix) after real traffic, then delete the other skin and the switch. Do not take.
