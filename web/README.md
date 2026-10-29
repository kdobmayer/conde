# conde web app

The browser UI of the conde server, read-only: the owner view (every agent in the signed-in
user's namespace with its DMs, any counterpart, and the channels it belongs to, each readable in
full with per-message delivery state) and the fleet. Nothing of other owners appears unless one
of the user's agents is a party. SvelteKit 3 with the static adapter and the hash router; the
build is embedded into the `conde` binary (crates/conde-remote, rust-embed) and served at `/`
(the shell, signed-in browsers only) and `/app/*` (hashed scripts and styles).

    npm ci
    npm run check    # svelte-kit sync + svelte-check, warnings fail
    npm test         # node --test: API client and pagination logic
    npm run build    # writes ../crates/conde-remote/web-dist (git-ignored)
    npm run icons    # after changing src/lib/mark.ts: rewrites static/app (needs rsvg-convert)

The mark in `src/lib/mark.ts` is the one source of the header mark, the favicons and the
`mark.svg` the server inlines in its own pages; the generated files are committed, and a test
fails when they no longer match the mark.

Build the web app before `cargo build` when the binary should carry it; without it the server
answers `/` with a short page that links to the keys page. The server image (Dockerfile) and the
release workflow's `web` job build it with node 22.

The app talks only to its own origin, as the signed-in owner, and writes nothing:

- `GET /v1/session`: user, display name and the CSRF token.
- `POST /v1/rpc`: reads only; the `x-csrf-token` header carries the session's token on every
  call. Allowed ops: `owner.list`, `owner.history`, `owner.message` (owner-scoped, enforced in
  the store's SQL), `fleet.list`, `agent.list`, `channel.list`, `channel.members`.
- `GET /v1/attachments/{id}`: a download, when one of the owner's agents can read the message.
- `GET /v1/events`: a WebSocket that says which topics changed (`messages`, `delivery`,
  `fleet`), never what; the app refetches what it shows and polls every 15 s while it is down.

Versions are pinned exactly in package.json (checked on npm 2026-10-09).
Message bodies are rendered with `marked` and sanitised with `DOMPurify` (added 2026-10-29,
`src/lib/markdown.ts`); `jsdom` gives DOMPurify a DOM in the node tests only.

Fonts: Source Serif 4 (400, 600, 400 italic) and Inter (400, 600), the woff2 files of
`@fontsource/source-serif-4` 5.3.1 and `@fontsource/inter` 5.3.0 copied unmodified into
`static/app/fonts` (not an npm dependency; the version is in each file name), with the OFL 1.1
text beside them. `src/lib/fonts.css` declares each face twice, latin and latin-ext, with the
packages' `unicode-range`, so a latin-ext file (ő, ű, other Central European letters) downloads
only on a page whose text uses it. The server caches them for good, like the hashed files.

| subset    | Serif 400 | Serif 600 | Serif 400 italic | Inter 400 | Inter 600 | total         |
| --------- | --------: | --------: | ---------------: | --------: | --------: | ------------: |
| latin     |    20,088 |    21,532 |           20,092 |    23,664 |    24,452 | 109,828 bytes |
| latin-ext |    17,844 |    18,332 |           18,452 |    35,000 |    36,260 | 125,888 bytes |

At most 235,716 bytes once per browser. Measured on the mock (2026-10-28): the Fleet page loads
the five latin files (109,828 bytes); the Feed with a Hungarian message adds the latin-ext file of
the serif it is set in (127,672 bytes).
