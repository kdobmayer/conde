# Web UI v2: implementation plan

Written so that any of the three directions can be chosen: a shared skeleton first (data, routes,
primitives, behaviour), then the direction's skin (shell layout, row and message components,
tokens, fonts). Each item below is one PR, in order; items marked "server" can run in parallel
with the client items that do not depend on them.

## Decisions taken here (for the owner to overrule)

- **Component library: none.** The app is about fifteen read-only components; the candidate
  libraries either bring their own visual system (Skeleton, Flowbite, shadcn-svelte: Tailwind
  plus their tokens, which would have to be fought to meet NEXT.md section D) or are headless
  primitives (bits-ui, Melt UI) whose value is in focus management for menus, dialogs and
  popovers. Cost of none: three accessible primitives written here (a filter menu with listbox
  keyboard behaviour, the phone sheet as a modal dialog, a disclosure for collapsed fleet owners),
  about one item of work (item 4) and the main accessibility risk of the plan. The alternative
  that keeps the option open is bits-ui for those three only; it adds a dependency to a binary
  that embeds its build, so it is taken only if item 4's menu fails its keyboard test.
- **Fonts are bundled** for the chosen direction (A: IBM Plex Mono; B: Source Serif 4 and Inter;
  C: Inter and IBM Plex Mono), self-hosted woff2, Latin subset, OFL. This is new to the app (today
  system stacks only). The alternative is system stacks, which keep the layout and lose part of
  the character (most for B).
- **Dark only** (owner, 2026-10-14): one token set, `color-scheme: dark`, no theme switch. The
  favicons keep their light and dark variants, since they live in the browser's chrome.
- **The design routes go, the harness stays.** `web/src/routes/_design/` is removed in item 2;
  the fixture, the mock API and the headless render script (`web/src/lib/design/fixture.ts`,
  `web/scripts/design/`) stay as the visual check every UI item renders against.
- **Keys stays a server page** with the new skin and nav (item 11), not a Svelte route.

## Order

| # | Item | Kind | Depends on |
|---|---|---|---|
| 1 | Conversation opens at the newest message | client fix | none |
| S1 | `owner.feed` op and `owner.search` in the browser allow-list | server | none |
| S2 | `owner.thread` op and the `reply_to` index | server | none |
| 2 | Shell: routes, navigation, tokens, dark only | client skeleton | 1 |
| 3 | Skin: the chosen direction's shell, tokens and fonts | client skin | 2 |
| 4 | Primitives | client skeleton + skin | 3 |
| 5 | Feed | client | 4, S1 |
| 6 | Thread view | client | 5, S2 |
| 7 | Conversation (DM and channel) rework | client | 4 |
| 8 | Agents and Channels pages | client | 7 |
| 9 | Fleet | client | 4 |
| 10 | Search | client | 5, S1 |
| 11 | Keys page skin and nav | server template | 3 |
| S3 | Channel history without naming an agent | server, optional | none |
| S4 | Owner-scoped event frames | server, optional | S1 |

## Items

### 1. Conversation opens at the newest message (defect, now)

The cause (audit §4): `refresh(first)` in `Conversation.svelte` scrolls to the end while
`loading` is true and only "Loading…" is rendered. Set `loading = false` before the scroll and
`await tick()` so the list is in the DOM. Test: a node test cannot see layout, so the proof is the
headless probe used in the audit (scrollTop equals scrollHeight minus clientHeight after load),
red on master, green on the branch, kept as `web/scripts/design/probe-scroll.mjs`.
Files: `web/src/lib/Conversation.svelte`, `web/scripts/design/probe-scroll.mjs`.
Independent of the overhaul; worth shipping on its own.

### S1. `owner.feed` (server)

`{before?|after?: seq, limit?, agents?, direction?, place?, with?, problems?}` → newest first,
`{messages, more, newest_seq}`; each message is the history view plus `delivery`, `via` (the
own agents that can read it) and `direction` (`in`, `out`, `own`). The visibility predicate is
the existing `owned` clause of `owner_search_scan`, ordered by `commit_seq DESC`. Add
`owner.feed` and `owner.search` to `BROWSER_OPS`.
Files: `crates/conde/src/ops/web.rs`, `crates/conde/src/ops/mod.rs`, `crates/conde/src/store/mod.rs`,
`crates/conde/src/store/sqlite.rs`, `crates/conde/src/store/postgres.rs`,
`crates/conde-remote/src/web/app.rs`, `crates/conde/tests/web_ui.rs`.
Tests on both stores (the suite's existing pattern): the owner sees their agents' DMs with anyone
and their channels' posts; a teammate's feed never contains a DM between two of another owner's
agents (the G9 rule); `after` returns exactly the newer rows; filters; paging stops at `more:
false`. Check the Postgres plan with the production row count (18,626 imported messages plus
later traffic); the `messages_by_sender`, `_recipient` and `_channel` indexes should serve it.

### S2. `owner.thread` and the `reply_to` index (server)

`{id}` → `{root, messages, hidden_parent}` under the same predicate; descendants by a recursive
query (both stores support `WITH RECURSIVE`). Migration 0012 adds `messages(reply_to)` on both
stores; there is no such index today.
Files: `crates/conde/src/store/migrations/0012_messages_by_reply_to.sql`,
`crates/conde/src/store/sqlite.rs`, `crates/conde/src/store/sqlite_schema.sql`,
`crates/conde/src/store/postgres.rs`, `crates/conde/src/store/mod.rs`, `crates/conde/src/ops/web.rs`,
`crates/conde/src/ops/mod.rs`, `crates/conde-remote/src/web/app.rs`, `crates/conde/tests/web_ui.rs`.
Tests: branch ordering, a parent visible only to the sender comes back as `hidden_parent`, a
teammate never receives another owner's chain.

### 2. Shell: routes, navigation, tokens, dark only (skeleton)

New route tree (hash router): `/` Feed, `/t/[id]`, `/agents`, `/a/[agent]`,
`/a/[agent]/dm/[owner]/[name]`, `/channels`, `/c/[name]`, `/fleet`; the old
`/a/[agent]/ch/[name]` redirects to `/c/[name]`. A `Nav` model (sections, own agents with state
from `owner.list` and `fleet.list`, channels deduped) in `board.svelte.ts`. Tokens move out of
`app.css` into `lib/ui/tokens.css` with neutral placeholder values; the light media query goes,
`color-scheme: dark`. Verify whether `page.url` exposes query parameters after the hash with the
hash router; if not, filters use a path segment (ia.md, Feed). Remove `routes/_design`; the
root layout drops its design branch.
Files: `web/src/routes/**`, `web/src/routes/+layout.svelte`, `web/src/lib/board.svelte.ts`,
`web/src/app.css`, `web/src/lib/ui/tokens.css`, `web/src/app.html`, `web/test/*.test.mjs`.

### 3. Skin: the chosen direction (skin)

The direction's `Shell.svelte` (A: top tabs and status line; B: masthead column; C: left rail),
its phone tab bar, its token values, and the bundled fonts with `@font-face` (measure the woff2
sizes and record them in `web/README.md`). Each option's `Frame.svelte` under
`routes/_design/<option>/` (in git history after item 2) is the starting point.
Files: `web/src/lib/ui/Shell.svelte`, `web/src/lib/ui/tokens.css`, `web/static/app/fonts/*`,
`web/README.md`.
Check: render the empty shell at 1440 and 390 against the option's PNGs.

### 4. Primitives (skeleton behaviour, skin styling)

`StateWord` (delivery state to word and tone), `Name` (own versus other, short versus
qualified), `RelTime`, `FilterMenu` (listbox semantics, arrow keys, Escape), `Sheet` (phone
dialog: focus trap, back closes, drag to expand), `SidePanel`, `SkeletonRows`, `EmptyState`,
`ErrorLine` (retry, "as of"), `LiveState`, `Announcer` (`aria-live`). C adds `Avatar` and
`CountTiles`.
Files: `web/src/lib/ui/*.svelte`, `web/test/ui.test.mjs` (pure logic: state mapping, names).

### 5. Feed (needs S1)

`lib/feed.svelte.ts`: pages newest first, `after` on `messages` events, merge by `message_id`,
refetch of the newest page on `delivery`, filters from the URL, the per-device last-seen `seq`
(browser storage, wrapped so it works without it), the "N new" marker. `FeedRow.svelte` in the
direction's form (A one line, B kicker plus two lines, C card). Teach the mock API `owner.feed`.
Files: `web/src/lib/feed.svelte.ts`, `web/src/lib/FeedRow.svelte`, `web/src/routes/+page.svelte`,
`web/src/lib/types.ts`, `web/scripts/design/mock-server.mjs`, `web/test/feed.test.mjs`.
Tests: merge and paging logic (node), a render at both widths.

### 6. Thread view (needs S2)

`Thread.svelte` (tree order and the re-label rule from `lib/design/model.ts`, moved to
`lib/thread-tree.ts` with tests), side panel on desktop, sheet on the phone, deep link
`#/t/<id>`, Escape and back close it, Feed scroll position kept.
Files: `web/src/lib/Thread.svelte`, `web/src/lib/thread-tree.ts`, `web/src/routes/t/[id]/+page.svelte`,
`web/scripts/design/mock-server.mjs`, `web/test/thread.test.mjs`.

### 7. Conversation rework

Split `Conversation.svelte` into a list and a `Message` component in the direction's form; one
delivery word per message with the detail on expand; reply context only when the parent is not
the message above (opens the thread); id in a per-message menu; opens at the newest (item 1);
"earlier" on scroll up. C adds the details panel.
Files: `web/src/lib/Conversation.svelte`, `web/src/lib/Message.svelte`, `web/src/lib/Delivery.svelte`,
`web/src/routes/a/[agent]/dm/[owner]/[name]/+page.svelte`, `web/src/routes/c/[name]/+page.svelte`,
`web/test/conversation.test.mjs`.

### 8. Agents and Channels pages

Agents index (phone), agent page with its conversations and status; Channels index and the
deduped channel route (passes the first own member agent until S3).
Files: `web/src/routes/agents/+page.svelte`, `web/src/routes/a/[agent]/+page.svelte`,
`web/src/routes/channels/+page.svelte`, `web/src/routes/c/[name]/+page.svelte`.

### 9. Fleet

Own hosts first, problems first, held and stuck as the only coloured items; teammates collapsed
per owner; legend as column headers. `types.ts`: `FleetAgent.address: string | null`.
Files: `web/src/routes/fleet/+page.svelte`, `web/src/lib/types.ts`.

### 10. Search

The Feed's search field calls `owner.search` (regex, oldest first, `next_cursor` paging); results
render as Feed rows and open their thread.
Files: `web/src/lib/feed.svelte.ts`, `web/src/routes/+page.svelte`, `web/scripts/design/mock-server.mjs`.

### 11. Keys page skin and nav (server template)

The `pages.rs` stylesheet takes the chosen tokens and font stack, the header gets the app's nav
(links to `/#/`, `/#/agents`, `/#/channels`, `/#/fleet`), the brand links to `/`, and the tables
become cards under 720 px. Form actions and CSRF unchanged.
Files: `crates/conde-remote/src/web/pages.rs`, its tests in the same file.

### S3 (optional). Channel history without naming an agent

`owner.history {channel}` resolves any own member agent. Removes the client-side choice in item 8.
Files: `crates/conde/src/ops/web.rs`, `crates/conde/tests/web_ui.rs`.

### S4 (optional). Owner-scoped event frames

Send `changed` to a browser only when the change touches its owner's agents, with the newest
`seq`. Removes refetches caused by other owners' traffic and the cross-owner timing signal
(audit §11). Needs the store event to carry the affected owners.
Files: `crates/conde-remote/src/web/app.rs`, the store event path in `crates/conde/src/store/`,
`crates/conde/tests/web_ui.rs`.

## What differs per direction

| | A Console | B Editorial | C Workbench |
|---|---|---|---|
| Shell (item 3) | top tabs, status line | masthead column | left rail with expandable agents |
| Feed row (item 5) | one-line table row | kicker + two lines of serif | card with initials and chips |
| Message (item 7) | log line, right-aligned speaker | script: speaker margin, serif text | bubbles, details panel |
| Extra components | none | none | Avatar, CountTiles, DetailsPanel |
| Fonts | IBM Plex Mono | Source Serif 4, Inter | Inter, IBM Plex Mono |
| Relative size | smallest | middle | largest (three columns, two extra panels) |

## Checks on every client item

`npm run check` (warnings fail), `npm test`, `npm run build`, and a headless render of the
touched pages at 1440×900 and 390×844 against the mock API (`scripts/design/shoot.mjs`), compared
with the chosen option's PNGs before review. Server items run the web suite on SQLite and
Postgres (`CONDE_TEST_PG`).
