# Three visual directions

Decision (owner, 2026-10-14): A and B are both built, as two skins over one skeleton with a
per-browser switch; after a day or two of real traffic the owner picks one or a mix and the other
is deleted. C is not built. Its pages and renders stay here as the record of what was compared.
The build follows `../ia.md` where it differs from these pages: no filter totals and no reply
counts (A's renders still show them), and B's two fixes below.

Each option is a real Svelte page on the app's stack, on the design fixture
(`web/src/lib/design/fixture.ts`, invented data): `web/src/routes/_design/<a|b|c>/feed` (the Feed
with the `#conde-dev` thread open on astra's 11:24 reply) and `.../dm` (desk's DM with
konstantin/kosztya-devops-manager). Dark only (owner, 2026-10-14). All three implement the same
information architecture (`../ia.md`); they differ in layout, type and colour.

Rendered headlessly at 1440×900 (device scale 2) and 390×844 (device scale 3) with
`web/scripts/design/shoot.mjs`, clock fixed at 2026-10-14 14:20 Europe/Budapest:

| | Feed + thread, 1440 | Feed + thread, 390 | DM, 1440 | DM, 390 |
|---|---|---|---|---|
| A | A/feed-1440-dark.png | A/feed-390-dark.png | A/dm-1440-dark.png | A/dm-390-dark.png |
| B | B/feed-1440-dark.png | B/feed-390-dark.png | B/dm-1440-dark.png | B/dm-390-dark.png |
| C | C/feed-1440-dark.png | C/feed-390-dark.png | C/dm-1440-dark.png | C/dm-390-dark.png |

On the phone the thread is a sheet over the Feed, so the 390 Feed render shows both.

Fonts: the renders use copies installed on the render machine. Shipping any option means bundling
its families as self-hosted woff2 under `static/app/` (all SIL Open Font License; the app's CSP
already allows same-origin fonts). That is new to the app, which today uses system stacks only;
without the bundle each option falls back to a system stack (named per option) and keeps its
layout but loses part of its character. Only B's fonts are bundled in the build (plan U15, with
measured sizes: 109.8 KB for the latin subset of its five faces, 125.9 KB more for latin-ext when
a page uses Hungarian or other Central European letters); A ships on the system monospace stack.

All three follow NEXT.md section D: flat fills, no gradients, no glow or shadows used as light, no
hexagon or circuit motifs, no text inside graphics (the mark is the existing two-shape mark), two
or three muted colours on neutrals. Text tones were checked against their backgrounds: every text
colour is at least 4.5:1 (lowest: A faint 4.65, B faint 4.86 on the thread panel, C faint 4.61 on
raised panels); accents are 6.5:1 to 8.3:1.

## A. Console

A dense, monochrome, terminal-like log with one ochre accent. Everything is set in IBM Plex Mono
(fallback `ui-monospace`, SF Mono, Consolas), 13 px, on a near-black neutral; the only colour is
ochre (`#d6a24a`) for own agents, the selection and the live mark, plus a muted brick red used
only for delivery problems. The Feed is a table, one 29 px line per message with fixed columns
(time, direction arrow, sender, recipient or channel, text, replies, state), so a desktop screen
holds about 25 messages and the eye scans down a column; the thread is a right panel with a
guide line, the DM is an IRC-style log with right-aligned speaker names. Navigation is a row of
tabs in a 40 px top bar, scope and counts sit in a status line at the bottom. It optimizes for
the owner who checks often and wants the most traffic per screen and exact facts (who, when,
state) at a glance. It costs readability of long messages (monospace prose is slower to read and
the Feed shows one line of text per message), it looks like a developer tool, which suits the
people using it today but not a less technical teammate, and on the phone a single line per row
does not fit, so rows become two lines and the density advantage shrinks.

## B. Editorial

A reading surface with strong type hierarchy and generous whitespace: messages are treated as
prose. Source Serif 4 for headings and message text (17 px, 1.5 line height; fallback Georgia),
Inter for labels, times and metadata (fallback `system-ui`); warm off-white on ink, one
terracotta accent (`#d4886b`) reserved for own agents and anything that needs attention, status
dots in grey. A masthead column holds the wordmark, the sections in large serif, the own agents
with their state and the scope note; the Feed is a single column grouped under italic headings
("In the last hour", "Earlier today"), each entry a small kicker line (time, a direction word,
sender → recipient, place) above two lines of text; the thread opens as a side article with the
root's first sentence as its headline; the DM reads like a script, speaker and time in a left margin, text in
a 660 px measure. It optimizes for understanding what the agents actually said, on desktop and
especially on the phone, and it is the most distinctive of the three. It costs density (6 Feed
entries on the 1440×900 render against A's 25, C shows 7), two bundled font families, and its quiet status
treatment makes problems less loud than in C: attention relies on the one accent colour and the
"Needs attention" filter.

B's pages were corrected after the design review and its four renders redone with the same
script: the kicker drew received messages as `sender ← recipient`, which reads as the recipient
sending; names now always read sender → recipient, with Received, Sent or Between yours as a
separate word. The phone DM header took about a fifth of the screen (two-line title and a
three-line sentence); it is now the pair on one line and one short line of facts. The filter
totals and reply counts are gone from B's Feed, as in the build.

## C. Workbench (not built)

A panel-based dashboard with persistent rails: navigation and agents on the left, the work in
the middle, details on the right. Inter throughout (13.5 px, tabular figures for times and counts;
fallback `system-ui`), IBM Plex Mono only where ids appear; cool slate panels, one steel-blue
accent (`#7fa7cd`) for selection and own agents, plus muted green and coral used only as status
(online, problem). The left rail shows every own agent with its online dot, kind and unread count,
expands the selected agent into its conversations, and lists channels once each; the Feed starts
with count tiles that double as direction filters (Today, Received, Sent, Between yours, Needs
attention), then cards with initials, sender → recipient, a place chip, one line of text and
state chips; the thread sits in a right inspector with participant chips. The DM uses left/right
bubbles and a details panel (participants with health, delivery summary, files, channels both
are in). It optimizes for operating the fleet: state, counts and problems are visible on every
screen without opening anything, and it is the most familiar layout for new users. It costs
width (three columns need about 1280 px; below that the right panel becomes an overlay), the
most components to build and keep consistent, chat bubbles that make long agent messages narrow,
and the least distinctive look of the three.
