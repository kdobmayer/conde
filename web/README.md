# conde web app

The browser UI of the conde server: conversations (DMs and channels with per-message delivery
state), the send box with @mention completion, and the fleet. SvelteKit 3 with the static adapter
and the hash router; the build is embedded into the `conde` binary (crates/conde-remote, rust-embed)
and served at `/` (the shell, signed-in browsers only) and `/app/*` (hashed scripts and styles).

    npm ci
    npm run check    # svelte-kit sync + svelte-check, warnings fail
    npm run build    # writes ../crates/conde-remote/web-dist (git-ignored)

Build the web app before `cargo build` when the binary should carry it; without it the server
answers `/` with a short page that links to the keys page. The server image (Dockerfile) and the
release workflow's `web` job build it with node 22.

The app talks only to its own origin, as the signed-in user's human:

- `GET /v1/session`: user, display name, the name the human writes as, and the CSRF token.
- `POST /v1/rpc`: dispatcher requests; the `x-csrf-token` header carries the session's token on
  every call. Ops used: `conversation.list`, `conversation.history`, `conversation.read`,
  `fleet.list`, `message.send`, `message.get`, `channel.list`, `channel.join`, `channel.create`.
- `GET /v1/attachments/{id}`: a download, under the message's visibility.
- `GET /v1/events`: a WebSocket that says which topics changed (`messages`, `delivery`,
  `fleet`), never what; the app refetches what it shows and polls every 15 s while it is down.

Versions are pinned exactly in package.json (checked on npm 2026-10-09).
