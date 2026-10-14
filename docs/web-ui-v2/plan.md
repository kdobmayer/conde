# Web UI v2: backlog items

Directions A (Console) and B (Editorial) are built as two skins over one skeleton with a
per-browser switch; C is not built; dark only (owner, 2026-10-14). What the pages show and the
server contract are in `ia.md`; each item below cites the section it implements. Items are sized
for the backlog: one PR each, taken top first, claimed on the thread with desk.

## Rules for every item

The BACKLOG.md rules apply (fresh main, one PR, one-line commit messages, no attribution,
`cargo test --workspace` green on both stores with CONDE_TEST_PG, no new clippy warning). In
addition:

- Never: a third skin or anything from C, a light theme or theme switch, a component library, a
  server-side skin setting, counts the contract drops (ia.md "Counts").
- Client items run `npm run check` (warnings fail), `npm test`, `npm run build`, and render the
  pages they touch with `web/scripts/design/shoot.mjs` against the mock API at 1440×900,
  1180×820 and 390×844, in both skin settings, the Feed with the thread closed and open. Renders
  go in the PR description. Headless only; nothing opens a browser window.
- Branches: U1 to U5 go to main (U1 is a live fix; U2 to U5 are additive browser-only reads).
  U6 to U16 replace the live UI page by page, so they land on an integration branch `web-v2`
  (from main after U1) and reach main in one merge after U16; main never ships a half-built UI.
  `web-v2` merges only after it has been rebased on a main that holds U1 to U5 and its gate
  (U16) has passed against the real ops on both stores, not only the mock.
- Parallel groups: items in the same group have disjoint areas and can run in parallel lanes;
  a group starts when the groups it names have landed. The server lane (S) is serial inside
  itself (shared store and op files) and parallel with every client group.

## Decisions taken here (for the owner to overrule)

- No component library. Three accessible primitives are written here (U8): a filter menu, the
  sheet (also the 720 to 1280 px overlay) and a disclosure. bits-ui is taken for the filter menu
  only if U8's keyboard checks cannot be met; it would add a dependency to a binary that embeds
  its build.
- Fonts: B bundles Source Serif 4 and Inter (U15); A ships on the system monospace stack
  (`ui-monospace, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace`) and bundles nothing.
  A is monospace throughout, so the system face keeps its character; B without its serif loses it.
- Search is a separate mode with the existing op; the Feed's agent filter takes one agent; the
  one-counterpart place filter and all totals and reply counts are dropped (ia.md "Counts",
  "Search").
- The thread sheet's handle is a button that toggles full height; no drag gesture.
- `/keys` keeps one look for both skins and takes the chosen accent after the pick.
- Invalidation without message ids: a `changed` frame carries topic bits only, and every change
  that can alter a Feed row arrives as `messages` or `delivery` (ia.md "Live update model"), so
  the client re-reads its loaded window on either topic, at most once per 2 s (ia.md "The Feed
  window").

## Items

- U1. A conversation opens at its oldest loaded message instead of the newest (audit §4, live
  defect): `refresh(first)` in `Conversation.svelte` scrolls to the end while `loading` is true
  and only "Loading…" is rendered. Set `loading = false`, `await tick()`, then scroll. Must not:
  touch anything else in the component. Gate: a headless probe on the DM route (after load,
  `scrollTop` equals `scrollHeight - clientHeight`), red on main, green on the branch, kept as
  `web/scripts/design/probe-scroll.mjs`. Area: web/src/lib/Conversation.svelte,
  web/scripts/design/probe-scroll.mjs. Group: any (ship on its own, now).

- U2. Browser op allow-list: add `owner.feed`, `owner.thread` and `owner.search` to `BROWSER_OPS`
  (the first two are registered by U3 and U5; until then they answer the dispatcher's unknown-op
  error). Must not: add any other op. Gate: a web suite case on both stores where a browser
  session's `owner.search` returns hits from its agents' DMs and channels and never a DM between
  two other owners' agents. Area: crates/conde-remote/src/web/app.rs,
  crates/conde/tests/web_ui.rs. Group: S (first).

- U3. `owner.feed` with lower and upper bounds (ia.md "Server contract", without `problems`):
  `{after?, before?, limit?, agent?, direction?, place?}` newest first, `{messages, more}`, each
  message the history view plus `direction` and `via`, visibility by the `owned` clause on both
  stores. Must not: a scan budget, a new deadline, counts, a `with` filter, owner-scoped events.
  Gate: web suite cases on both stores: the G9 rule (a teammate's feed never holds a DM between
  two other owners' agents); both bounds exclusive and `more` exact; a drain of more than two
  pages with `after` fixed and `before` moving returns exactly the rows above `after`; a row
  committed between two page calls is returned by the next pass from the same `after`; each
  predicate and agent + direction + place combined; the three direction cases. Cost gate, with
  the query plan on both stores (EXPLAIN QUERY PLAN on SQLite, EXPLAIN ANALYZE on Postgres) in the
  PR: the unfiltered and agent-filtered first page at the production row count; a sparse owner
  (a few dozen messages) on a dense board (the production count) reads by index, not the board;
  a sustained burst at the maximum window (10 pages of 50 re-read every 2 s for a minute) with
  the server time per pass recorded. Area: crates/conde/src/ops/web.rs,
  crates/conde/src/ops/mod.rs, crates/conde/src/store/mod.rs, crates/conde/src/store/sqlite.rs,
  crates/conde/src/store/postgres.rs, crates/conde/tests/web_ui.rs. Group: S (after U2).

- U4. The `problems` predicate of `owner.feed`, server-side: candidates from `notifications` by
  status (held, denied, refused, gone, pending with attempts), exact state per candidate as
  `recipient_state` computes it, over the same recipients `delivery()` shows the owner (all of
  them when the sender is own, only own recipients otherwise), no scan budget. Must not: filter
  on the client, count, judge another owner's recipient. Gate: web suite cases on both stores:
  each problem state listed; a held DM that was then read is not; a message from another owner's
  agent whose only problem is at another owner's recipient is not; a page with many non-problem
  rows between two problems returns both and never an empty page with `more: true`; no problems
  answers an empty page with `more: false`. Cost gate, plans on both stores in the PR: thousands
  of candidates (denied, refused, gone, and held then read) with zero exact hits, answered empty
  with `more: false`, time recorded. Area: same as U3. Group: S (after U3).

- U5. `owner.thread` with bounds and the `reply_to` index (ia.md "Server contract",
  "Thread view"): the subtree under `top` (the segment's top, or the selected message's 99th
  ancestor when the path is longer than 100, with `start`); the first answer selects the path
  plus the subtree's first messages by seq, 200 in all, and returns the union sorted by seq;
  `next` continuation; `hidden_parent`; the visibility predicate inside every recursive step;
  migration 0013 adds `messages(reply_to)` on both stores. Must not: return or count a message
  the owner cannot read, step through one. Gate: web suite cases on both stores: a chain
  A ← B ← C ← D with B invisible gives `thread(C)` = C, D with `hidden_parent` and `thread(A)` =
  A only; a teammate's id answers not found; a 250-message chain opened at its 230th message
  starts at its 131st with `start` naming the first; a long branched thread (a path of 150 with
  300 replies on a side branch) opened deep in the path: the selected id is in the first answer,
  and after continuation and a merge by id every subtree message is present once and every
  parent is present or is `top`. Cost gate, plans on both stores in the PR: that branched thread
  on the production row count, time per answer recorded. Area:
  crates/conde/src/store/migrations/0013_messages_by_reply_to.sql,
  crates/conde/src/store/sqlite_schema.sql, crates/conde/src/store/sqlite.rs,
  crates/conde/src/store/postgres.rs, crates/conde/src/store/mod.rs, crates/conde/src/ops/web.rs,
  crates/conde/src/ops/mod.rs, crates/conde/tests/web_ui.rs. Group: S (after U4).

- U6. App shell, routing and the skin switch (ia.md "Two skins, one skeleton", "Navigation"): the
  route tree (`/`, `/t/[id]`, `/agents`, `/a/[agent]`, `/a/[agent]/dm/[owner]/[name]`,
  `/channels`, `/c/[name]`, `/fleet`; old `/a/[agent]/ch/[name]` redirects to `/c/[name]`) with
  placeholder pages; the skin registry (`Shell`, `FeedRow`, `Message` per skin, both pointing at
  plain skeleton components until U12 and U13); token files per skin under `:root[data-skin=...]`
  with the option values; `color-scheme: dark`, the light media query removed; the root layout
  reads `conde.skin` (try/catch, default A) and sets `data-skin`; the Console / Editorial control
  in the owner menu, which swaps only the skin components (the data layer is not remounted; the
  first visible row keeps its offset); the nav model (sections, own agents with state from
  `owner.list` and `fleet.list`, channels once each). Verify whether the hash router exposes query
  parameters after the hash; if not, filters use the path form. Must not: a server setting, a
  light theme, touch `routes/_design` or its layout branch. Gate: a node test of the skin read
  (missing, unknown, invalid and throwing storage give A); renders of the empty shell in both
  skins. Area: web/src/routes/+layout.svelte, web/src/routes/+page.svelte,
  web/src/routes/{t,agents,a,channels,c,fleet}/**, web/src/lib/skins/**,
  web/src/lib/nav.svelte.ts, web/src/lib/UserMenu.svelte, web/src/app.css, web/src/app.html,
  web/test/skin.test.mjs. Group: C1 (after U1; parallel with U7, U8).

- U7. Data layer with the window rule (ia.md "The Feed window", "Search"): the window logic as a
  pure module driven by an injected op and clock (`lib/feed-window.ts`: `floor` and `exhausted`
  kept apart from rendered rows, fresh window, older page, refresh pass, empty-after-pass
  restart, the 10-page bound reusing the pass's first page, one pass per 2 s with a dirty
  re-run); its reactive wrapper (`lib/feed.svelte.ts`: passes on reconnect, `messages` and
  `delivery` frames and poll ticks through the existing `onBoard`, a filter change starts a
  fresh window, filters from the URL, the per-device last-seen seq in storage inside try/catch,
  "N new", search mode); the thread answer with continuation merged by id
  (`lib/thread-view.svelte.ts`); tree order and the re-label rule (`lib/thread-tree.ts`, from
  `lib/design/model.ts`); `types.ts` for the new answers and `FleetAgent.address: string | null`;
  the mock API answers `owner.feed`, `owner.thread`, `owner.search` per the contract over the
  fixture. Must not: merge the Feed list by id (the window replaces it), let a bounded call set
  `exhausted`, decide visibility on the client. Gate: node tests: a gap of more than two pages
  drained, watermark unchanged until the last page; an arrival during a pass caught by the
  re-run; triggers inside 2 s give one pass and one re-run; a held row on the second loaded page
  turns read after one pass; a resolved problem leaves the list; every loaded problem resolved
  while an older one exists below `floor` gives a fresh window that shows it, never "No delivery
  problems"; an empty answer to a fresh window with `more: false` reads as none and a failed call
  reads as an error with the window kept; a pass at the tenth page with `more: true` restarts
  from its first page with exactly 10 calls; a thread with `hidden_parent` and a continuation
  with no holes. Area: web/src/lib/feed-window.ts, web/src/lib/feed.svelte.ts,
  web/src/lib/thread-view.svelte.ts, web/src/lib/thread-tree.ts, web/src/lib/types.ts,
  web/scripts/design/mock-server.mjs, web/test/feed.test.mjs, web/test/thread.test.mjs. Group:
  C1 (parallel with U6, U8).

- U8. The three primitives with interaction checks, and the small state components:
  `FilterMenu` (button and listbox: Enter, Space and ArrowDown open, arrows move, Enter selects,
  Escape closes and returns focus to the button), `Sheet` (modal dialog: bottom sheet under
  720 px, right overlay from 720 to 1280 px; focus moves in, Tab stays in, Escape and browser back
  close it, focus returns to the opener, the page behind is inert; the handle button toggles full
  height), `Disclosure` (button with `aria-expanded`); `StateWord` (state to word and tone, the
  mapping a pure function), `EmptyState`, `ErrorLine` (Retry, "as of"), `SkeletonRows`,
  `Announcer` (`aria-live`). A bare harness page renders the primitives on fixture data;
  `interact.mjs` drives it headless by keyboard at 1440 and by touch at 390 and exits non-zero on
  a failed check. Must not: a component library unless the FilterMenu checks fail (then bits-ui
  for that menu only, named in the PR), animation without a reduced-motion guard. Gate:
  `interact.mjs` green (output in the PR); a node test of the state-word mapping. Area:
  web/src/lib/ui/**, web/src/routes/_design/primitives/+page.svelte,
  web/scripts/design/interact.mjs, web/test/ui.test.mjs. Group: C1 (parallel with U6, U7).

- U9. Feed and thread pages (ia.md "Feed", "Thread view"): the list with the skin's `FeedRow`,
  day groups, the last-visit divider, the "N new" marker, the filter bar, search mode, all
  states; the thread beside the Feed from 1280 px, in the `Sheet` below; `#/t/<id>` deep links;
  close keeps the Feed scroll position. Adds the 1180×820 size to `shoot.mjs`. Must not: show
  totals or reply counts. Gate: renders per the rules; `interact.mjs` extended to the Feed:
  opening a row and pressing Escape returns focus to that row at the same scroll position, back
  closes the sheet at 390; switching the skin on a filtered, scrolled Feed with a thread open
  keeps the URL, the filters, the open thread and the first visible row, and makes no
  `owner.feed` call. Area: web/src/routes/+page.svelte, web/src/routes/t/[id]/+page.svelte,
  web/src/lib/Thread.svelte, web/src/lib/FeedList.svelte, web/scripts/design/shoot.mjs,
  web/scripts/design/interact.mjs. Group: C2 (after C1; parallel with U10, U11).

- U10. Conversation, Agents and Channels pages (ia.md "DM", "Channels", "Agents"): the
  conversation as a list of the skin's `Message`, one delivery word per message with the detail
  (and the message id with a copy button) on expand, reply context only when the parent is not
  the message above, opens at the newest, earlier on scroll up; the Agents index and agent page;
  the Channels index and the channel route through the first own member agent. Must not: print
  the message id inline, a per-message menu. Gate: renders of a DM and a channel per the rules;
  `probe-scroll.mjs` still green. Area: web/src/lib/Conversation.svelte,
  web/src/lib/Delivery.svelte, web/src/routes/a/**, web/src/routes/agents/**,
  web/src/routes/c/**, web/src/routes/channels/**, web/test/conversation.test.mjs. Group: C2
  (after C1; parallel with U9, U11).

- U11. Fleet page (ia.md "Fleet"): own hosts first, problems first, held and stuck the only
  coloured items, teammates collapsed per owner in a `Disclosure`, column headers instead of the
  legend paragraph. Must not: show an address or delivery health for other owners' agents. Gate:
  renders per the rules. Area: web/src/routes/fleet/**. Group: C2 (after C1; parallel with U9,
  U10).

- U12. Skin A, Console (options/README.md "A"): `Shell` (top tabs, status line with the live
  state and the scope note, phone tab bar with Keys), `FeedRow` (one table line at 720 px and
  wider, two lines below), `Message` (log line, right-aligned speaker), tokens, the system
  monospace stack. The design renders show filter totals and reply counts; the build drops them.
  Must not: bundle a font, touch skeleton files. Gate: renders per the rules, compared with
  options/A; `interact.mjs` green. Area: web/src/lib/skins/a/**. Group: C3 (after C2; parallel
  with U13, U14).

- U13. Skin B, Editorial (options/README.md "B"): `Shell` (masthead column, phone tab bar with
  More), `FeedRow` (kicker: time, direction word, sender → recipient always in that order, place;
  two lines of text), `Message` (script with the speaker margin), tokens; the phone DM header is
  two short lines. The arrow is a small inline SVG, not a glyph (neither shipped font subset has
  U+2192). Uses the fallback stacks (Georgia, `system-ui`) until U15. Must not: point the arrow by
  direction, touch skeleton files. Gate: renders per the rules, compared with options/B;
  `interact.mjs` green. Area: web/src/lib/skins/b/**. Group: C3 (parallel with U12, U14).

- U14. Keys page restyle (ia.md "Keys"): the skeleton's dark neutrals and type in the `pages.rs`
  stylesheet, the app's nav in its header (links to `/#/`, `/#/agents`, `/#/channels`,
  `/#/fleet`), the brand links to `/`, the tables become cards under 720 px with state and Revoke
  visible; the mock's copy of the template follows. Must not: change the CSP, form actions or
  CSRF, follow the skin switch, add an op. Gate: the page tests in `pages.rs` (nav links present,
  forms unchanged); renders of `/keys` at 1440 and 390 with no horizontal scroll at 390. Area:
  crates/conde-remote/src/web/pages.rs, web/scripts/design/mock-server.mjs. Group: C3 (parallel
  with U12, U13).

- U15. Fonts for skin B: Source Serif 4 (400, 600, 400 italic) and Inter (400, 600) as woff2,
  copied unmodified from `@fontsource/source-serif-4` 5.3.1 and `@fontsource/inter` 5.3.0 (files,
  not an npm dependency), latin and latin-ext subsets each with its `unicode-range`, so the
  latin-ext files (Hungarian ő and ű, Central European names) download only on a page that uses
  them; `font-display: swap`; file names carry the version. Measured on 2026-10-14: latin 109.8 KB
  for the five faces (20,088 + 21,532 + 20,092 + 23,664 + 24,452 bytes), latin-ext 125.9 KB
  (17,844 + 18,332 + 18,452 + 35,000 + 36,260), at most 235.7 KB once per browser. Licence: SIL
  Open Font License 1.1 for both; Source Serif reserves the name "Source", which is why the
  published subsets ship unmodified instead of our own subset; the OFL text ships beside the
  files. One fixture message carries Hungarian accents (á é í ó ö ő ú ü ű). Must not: bundle a
  font for A, subset or modify the files. Gate: a B render of the
  Hungarian message where `document.fonts.check` is true for every face on "őű", and the font
  bytes the render transferred, both in the PR; sizes recorded in `web/README.md`. Area:
  web/static/app/fonts/**, web/src/lib/skins/b/fonts.css, web/src/lib/design/fixture.ts,
  web/README.md. Group: after U13.

- U16. Remove the design routes and gate `web-v2` on the real server:
  `web/src/routes/_design/**` (the A, B, C option pages and the primitives harness),
  `lib/design/model.ts`, and the root layout's design branch go; `interact.mjs` targets the real
  Feed and Fleet pages instead of the harness. The fixture, the mock API, `shoot.mjs`,
  `probe-scroll.mjs` and `interact.mjs` stay as the checks every UI change runs. A web suite
  smoke case on both stores serves the built app and runs the client's own calls against the
  real ops: load the shell and the Feed's first page (`owner.feed`), open a thread on one of its
  rows (`owner.thread`), filter problems (`owner.feed {problems: true}`), search
  (`owner.search`), asserting the fields the client reads. Must not: remove the harness scripts
  or the fixture. Gate: the smoke case green on both stores; `interact.mjs` green against the
  real pages; renders unchanged. Area: web/src/routes/_design/**,
  web/src/routes/+layout.svelte, web/src/lib/design/model.ts, web/scripts/design/interact.mjs,
  crates/conde/tests/web_ui.rs, docs/web-ui-v2/options/README.md. Group: last, after U1 to U5
  are on main and U6 to U15 on `web-v2` (rebased on that main); then `web-v2` merges to main.

## Waiting on a decision (do not take)

- The owner's pick between A and B, or a mix, after a day or two of real traffic. Then one item
  deletes the other skin's directory and the switch and gives `/keys` the chosen accent.
