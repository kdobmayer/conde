# conde design v2: a local board on every machine, explicit shared participation

2026-10-09. Model decided by the owner the same day (Astra's proposal, `local-and-shared-boards`;
the earlier DNS-style search-list sketch is superseded where it differs). This file states the
design to build; v1 (docs/DESIGN.md) stays the description of what is live. Nothing here is
implemented yet. Revised the same day with Astra's design review (docs/DESIGN-v2-astra-review-1.md);
this revision is the V0 contract the implementation lanes build on. The delivery contract (bounded
lossless admission, the restart contract) was completed after Astra's second pass
(docs/DESIGN-v2-astra-review-2.md), and the per-origin receipt budget after the third
(docs/DESIGN-v2-astra-review-3.md).

## 1. Goal and non-goals

Goal: the owner gets kond's local collaboration back (a board on his machine that works with the
network unplugged and from which nothing leaves the device) while selected agents keep a shared
address on the central board, so teammates' agents and his exchange messages as they do today.
Every cross-board action names its destination, and a reply always has a return address.

Non-goals for v2:
- the owner's private cross-machine board (desktop to laptop): deferred;
- several workspaces with their own membership, tenancy: deferred (the central board is one workspace);
- secrecy from the server operator (encryption): deferred;
- an offline cache of shared history, a local web UI, mirroring channels between boards;
- reply-only (NAT-style) records for unpublished agents: rejected (section 3, rule 4);
- isolating a dual-participating session: publication controls routing, not what an LLM that sees
  both boards repeats. An agent that must not carry local context outward is run as a separate,
  shared-facing session.

## 2. The model

```
 CLI / MCP (actor = harness address, derived as in v1)
        |
  conde daemon (one per OS user per machine)
   |-- resolver: picks exactly one board per request
   |-- local board: SQLite conde.db, local registry, channels, cursors (the v1 local mode, unchanged)
   |-- delivery: one notifier (one Claude reply socket), local jobs + shared jobs, receipts routed back
   `-- shared connections, 0..N: one BridgeClient each (v1 conde-remote), credential per board
          |
   shared board (v1 server, Postgres): mailboxes owner/name bound to (host), never to a session
```

- **Board**: one store with its own messages, channels, cursors, receipts and idempotency records.
  The local board is the v1 local-mode board. A shared board is the v1 server board. A board has a
  local alias (`local` is reserved); the first shared board is the central board, suggested alias
  `dakai`. An alias is a local name pinned at enrollment to one server and one account (section 4),
  never an identity or a permission.
- **Local registry**: names on the local board bound to harness addresses (`claude:<session>@host`),
  exactly the v1 rules (self-only registration, pins, rename). `conde register` always writes here.
- **Mailbox**: an agent row on a shared board, `owner/name`, bound to the address
  `mailbox:<mailbox_id>@<host>` where `mailbox_id` is that row's own `agent_id`. The server knows the
  name, the host and whether the mailbox is currently available; never a session or thread id. Humans
  publish their human name; its address stays `human:<user>@<host>` (no session involved, never pushed to).
  The row's `binding_version` is the mailbox's **route generation**: it changes on every publish,
  takeover and unpublish, and every notify carries it.
- **Publication**: a persisted, explicit binding on the local board between one local agent and one
  mailbox on one shared board. Only the local daemon maps the mailbox to a session, through the local
  agent's current binding, so a session replacement (the name re-registered from a new session) keeps
  the publication. Created only by `conde publish`; host enrollment, login and session discovery
  create none. Its lifecycle runs as a local operation record that survives a crash (section 5).
- **Provenance**: every message, id, reply-to, attachment, search hit, receipt, cursor and
  idempotency record lives on exactly one board and is shown with it. Shared message ids are printed
  qualified, `dakai:01M4...`; bare ids are local. Delivery bookkeeping is keyed by (board, board-local
  id), never by an id alone.

What each side can see: the shared board holds the published mailboxes, their host and
availability, and the shared traffic. The local board holds everything else, and nothing of it
crosses to a server.

## 3. Resolution rules (the resolver, new in the daemon)

Problem it solves: v1 cloud mode forwards every request to the server, so a name typo or an outage
decides where a message goes. The resolver makes the board a property of the request.

1. Board of a request: `--board <alias>` if given; else the board of a qualified message reference in
   the request (`read`, `show`, `attachment`, `react`, `send --reply-to`); else, for names and
   channels, the host's default board (decision 9: set once by the human at `conde setup`, `local`
   unless changed); else local. A reply therefore inherits its conversation's board. A bare message
   reference always means local, in the CLI and in MCP alike, whatever the default board.
2. Conflicts are errors, never guesses: `--board dakai` with a reference qualified for another board,
   or with a bare (local) id: `01M4... is a local message; drop --board, or use the dakai: id`.
3. Names and channels are looked up on the selected board only. A miss is an error naming that board
   (`no agent 'reviewer' on the local board; shared boards are addressed with --board <name>`). The
   daemon sends no frame to any server for a request resolved to local, lookups included.
   `register`, `unregister`, `rename` and `pin` act on the local registry only. The default board does
   not apply to them, and an explicit `--board` naming a shared board is rejected
   (`register is local; a shared address is conde publish <name> --board dakai`).
4. Sending, reading or reacting on a shared board acts as the caller's mailbox there. A local agent
   without a publication on that board gets: `worker is not published on dakai. Publication is set by
   your human: conde publish worker --board dakai`. A publication that is `publishing` or
   `unpublishing` is not usable outbound either. No reply-only identity is created.
5. Unqualified names on a shared board resolve in the caller's owner namespace on that board (v1).
   `@mentions` resolve on the board of the message; nothing crosses boards.
6. Channels belong to one board. Local `#dev` and dakai `#dev` are distinct; nothing is mirrored.
7. Aggregate views (`inbox`, `sent`, `read` with no target, `whoami`, `boards`, `doctor`)
   query the local board and each connected board where the caller is published, merge by time,
   and label every row with its board. Each row's read state, receipt and cursor stay on its own
   board. `--board X` on an aggregate view shows board X only (`--board local`: the local part only);
   the default board does not narrow an aggregate view. Timing: an operation resolved to local never
   waits on a board. An aggregate view queries the boards in parallel and waits at most 2 s for them,
   then shows a board that has not answered as `dakai: unavailable`; `--board local` answers with
   no wait. Everything else uses rules 1 to 6.

## 4. Data model deltas

### Local board (SQLite only)

Two new tables, created in `sqlite_schema.sql`. They are reached through a SQLite-only extension of
the transaction (`LocalTx`), not the shared `Tx` trait, because no server ever holds them.

```sql
CREATE TABLE IF NOT EXISTS publications(
    board TEXT NOT NULL,                 -- local alias of the shared board ('dakai')
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),   -- the local agent
    shared_name TEXT NOT NULL,           -- name part of owner/name on that board
    mailbox_id TEXT,                     -- the board's agent_id; NULL while 'publishing'
    route_generation INTEGER,            -- the mailbox's binding_version on the board
    state TEXT NOT NULL CHECK (state IN ('publishing', 'published', 'unpublishing')),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (board, agent_id),
    UNIQUE (board, mailbox_id),
    UNIQUE (board, shared_name)
);
-- One row per remote mutation, written before the first remote call, so a crash resumes it.
CREATE TABLE IF NOT EXISTS board_ops(
    op_id TEXT PRIMARY KEY,              -- ULID, also the idempotency key sent to the board
    board TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('publish', 'unpublish', 'migrate')),
    agent_id TEXT,                       -- publish / unpublish
    request_json TEXT NOT NULL,          -- the exact body sent (shared name, expected generation, manifest)
    state TEXT NOT NULL CHECK (state IN ('pending', 'done', 'failed')),
    result_json TEXT,                    -- the board's answer, kept for audit
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

At most one `pending` op exists per (board, agent), and one `migrate` op per board.

Board configuration stays in files, one per board, mode 0600 in a 0700 directory:
`$XDG_CONFIG_HOME/conde/boards/<alias>.json` = `{server, host_id, owner, key}` (the v1 credential
format plus the account name from `board.info`, named by alias). The v1 file `conde/credentials` is
moved there on first start. An alias stays pinned to its server and account: enrolling or logging in
under an existing alias with a different server or account is refused (`dakai is pinned to
https://conde.nativeai.space as kdobmayer`). Retargeting means `conde boards --forget <alias>`, which
deletes the local file and the alias's publications and ops; it never touches the server. A stable
board id in `board.info` is not needed while one shared board exists. The local registry, channels
and messages keep the v1 schema.

### Shared board (both backends: Postgres migration `0006_mailbox.sql`, SQLite schema + upgrade)

| Change | Postgres | SQLite |
|---|---|---|
| `agents.address_kind` accepts `'mailbox'` | `ALTER TABLE agents DROP CONSTRAINT agents_address_kind_check, ADD CONSTRAINT agents_address_kind_check CHECK (address_kind IN ('claude','codex','human','mailbox'))` | new files: the CHECK in `sqlite_schema.sql`; existing files: the standard table rebuild of `agents` in one transaction with foreign keys off, run once by `sqlite.rs` (local boards never write `mailbox`, the rebuild keeps one schema) |
| `hosts.mailbox_only`: set once the host migrated to v2; from then the server refuses harness addresses and harness endpoints from that host and refuses a v1 hello | `ALTER TABLE hosts ADD COLUMN mailbox_only BOOLEAN NOT NULL DEFAULT false` | `ALTER TABLE hosts ADD COLUMN mailbox_only INTEGER NOT NULL DEFAULT 0` (the existing ALTER path) |

No other table changes. A published mailbox is an ordinary agent row, so messages, channels,
memberships, notifications, attempts, fencing and idempotency work unchanged. Receipts keep the v1
statuses, but a mailbox row hides which harness is behind it: the daemon normalizes harness outcomes
before reporting them (section 5, outcome contract). Pins are a local-board feature; a mailbox row
has none. v1 idempotency rows are never purged, which the `migrate` replay relies on.

### Code-level types

- `address::Kind::Mailbox` (`mailbox:<agent_id>@<host>`).
- `conde_remote::Endpoint::Mailbox { mailbox_id }`.
- `hello` gains `"proto": 2` and `"window": W`, the admission window (section 5); a v2 hello lists
  only `Mailbox` endpoints.
- `Notify` gains `kind` (dm | mention | post), `sender`, `place`, `badges`, so the daemon renders the
  notification text with the board alias. They are required on a v2 connection; `text` stays for v1
  bridges only, and a v2 daemon never injects it.
- New server-to-daemon frame `routes {mailboxes: [{mailbox_id, route_generation}]}`: this host's
  current routes, sent on connect and after any change to them (takeover by another host, unpublish,
  admin unbind).
- The outcome `reason` is a closed enum on a v2 connection (section 5).

## 5. The daemon

`conde daemon` has one shape: the v1 `run_local` (local dispatcher, unix socket, bridge loop) plus
one BridgeClient per file in `conde/boards/`. `run_cloud` and the `Backend` enum go away; `--server`
is accepted and ignored with a warning for one release, because v1 service units pass it (`conde
setup` rewrites the unit). Startup binds the socket first (one daemon per machine, as in v1), opens
the local board and starts local delivery before any network call, so nothing local waits on a server.
A board connection that is fenced, has its credential revoked or keeps failing stops that connection
only; the daemon and the local board keep running, and `doctor` names the board's state. In v1,
`run_cloud` exits the whole process in those cases.

**Resolver and actor translation.** Every CLI/MCP request goes through the resolver (section 3). A
local request is dispatched exactly as in v1. A shared request is translated before it leaves:
the local registry maps the actor address to the local agent, a publication in state `published`
gives the mailbox, and the forwarded `actor` is `mailbox:<mailbox_id>@<host>` (`human:<user>@<host>`
for a human); `from` is dropped. A request carrying a harness address never leaves the machine.

**Publication** (the operation record of section 4 makes each step resumable).
- `publish <agent> --board X [--as <name>] [--take]`, human only:
  1. one local transaction writes a `board_ops` row (`pending`, its `op_id` is the idempotency key)
     and the publication row in state `publishing`;
  2. `mailbox.publish` on X with that key;
  3. one local transaction stores `mailbox_id` and `route_generation`, sets the state to `published`,
     and marks the op `done`.
  After a crash or a lost response, the daemon resumes every `pending` op on its next connect with
  the same key, and the server replays its stored answer. While a publish op is pending on a board, a
  notify for a mailbox the daemon does not know yet is reported `failed` with reason
  `recipient unavailable`, which the board retries after its backoff, never `refused`. Publishing
  needs the board online: when it cannot be reached, the command reports that the op is pending, and
  `doctor` shows it until it completes. Publishing again under the same shared name is a no-op. A
  different `--as` while published is refused (`worker is published on dakai as worker; unpublish
  first`). A second publish or unpublish while one is pending for the same agent is refused and
  names the pending op.
- `unpublish <agent> --board X`: the human, or the agent itself (shrinking exposure is always
  allowed). One local transaction writes the op and sets the row to `unpublishing`, which stops
  inbound admission and outbound use at once. Then `mailbox.unpublish {mailbox_id,
  expected_generation}` runs. On success the row is deleted. A generation mismatch means the mailbox
  moved meanwhile: the local row is deleted and the new owner's route is untouched. A failed call
  stays pending and is resumed on every (re)connect. A notification already injected cannot be
  recalled.
- Reconciliation: on connect (`mailbox.mine`) and on every `routes` frame, the daemon updates its
  rows before it injects any further job from that connection. A local publication the board no
  longer routes here (taken by another host, unpublished, unbound by an admin) is deleted and reported
  by `doctor`. A route generation newer than the local one is adopted.

**Inbound.** A shared notify is admitted only if all of these hold, checked at the moment of
injection rather than when the job was queued:
- it names `Endpoint::Mailbox` and carries the structured fields;
- a publication on that board with that mailbox id is in state `published`;
- `Notify.binding_version` equals that publication's route generation;
- its local agent is bound.

Admitted: the text is rendered from the structured fields with the alias (`conde [dakai]: new message
dakai:01M4... from balazs-rozsenich/relay. Read it with: conde read dakai:01M4...`) and pushed by the
one notifier. A harness endpoint on a shared connection, a notify without the structured fields, an
unknown or `unpublishing` mailbox, a stale generation, or a human mailbox is reported `refused` with
reason `recipient unavailable`, which ends the notification on the server. (The exception is an
unknown mailbox while a publish op is pending, see above.) A published agent whose local name is not
bound to a session reports `failed` with reason `recipient offline` (retried after the v1 backoff),
so its messages wait in the mailbox. The message itself is not copied into local SQLite: it stays
authoritative on its board, and `conde read dakai:...` fetches it from there.

Race boundary, stated honestly: revocation takes effect at the old host when it applies the `routes`
frame (or reconnects). A job that the old host injected before that moment cannot be recalled. This
is a message the persona would legitimately have received before the move. Takeover is complete
for routing when the server commits the new generation, and complete for the old host when it applies
the frame.

**Delivery multiplexer** (new, replaces the two v1 delivery loops). Problem: the Claude reply socket
exists once per process, so local and shared deliveries must share one `HarnessNotifier`. Without
isolation, a slow board would stall local pushes: in v1 the cloud delivery loop sleeps while a
board's outcome budget is full.
- **One thread owns the notifier.** It takes jobs from one bounded queue per origin (local, and
  each board), round-robin with one job per origin per turn. It never waits on a remote call, a
  reconnect, a report budget or an acknowledgement, and it holds no SQLite transaction across
  any of those.
- **Admission window: the server holds the backlog.** The v1 transport cannot re-offer a job:
  - the server pushes a pending attempt once per connection (its `sent` set);
  - the client marks an attempt seen before the callback, and that mark survives reconnects;
  - the callback channel is unbounded.

  Pausing after the callback would therefore hide an unbounded backlog, and dropping a job would
  strand it. V2 bounds the backlog where it is durable instead, on the server:
  - **The window.** The v2 hello declares a window W (default 32). An attempt counts against it
    from its push until the server records its first outcome; on the server that is the attempt
    row in status `dispatching` for this bridge instance, so the count survives reconnects. The
    server claims new attempts for a v2 instance only up to `W - dispatching`. Everything beyond
    stays pending in the board's database, which is lossless and needs no memory on the host. A v1
    bridge has no window and behaves as today.
  - **Retention invariant.** An attempt the client has marked seen is retained by the daemon until
    an outcome for it has been handed to the client: in the board's job queue (capacity W), then in
    the token map once injected. Nothing seen is ever dropped. A job that admission refuses (route
    revoked, publication gone) is settled with `refused` or `failed`, never discarded. A notify the
    daemon already holds or already injected (by board and attempt id) is ignored. This second
    de-duplication is backed by the token map, so it does not depend on the client's seen-set
    capacity.
  - **Outcomes and receipts.** Outcomes and late receipts never block the notifier thread. They go
    into a per-board map keyed by token, where the latest status wins (a terminal status is never
    replaced by a non-terminal one), and the board's task drains
    the map into the client's outcome book (`bridge.report`) whenever the book has room. A
    terminal outcome stays in the map until the client has accepted it into its book (from then
    on the client owns it, under the restart contract below).
  - **Receipt budget: one reservation per unresolved attempt.** The window frees on the first
    outcome, so `accepted` and `held` attempts are not bounded by it. v1's notifier cannot be the
    bound either: `harness.rs` keeps one global, evicting `msg_id -> attempt` cache (`REMEMBERED`,
    the oldest entry is popped whatever its state, and a receipt for a popped entry is dropped).
    So:
    - each origin (local, and each board) has its own receipt budget R (default 256; local
      independent of every board);
    - the notifier thread reserves one slot of the job's origin before injecting it;
    - the slot, the daemon's token-map entry and the notifier's mapping are held together, and
      nothing evicts them: the notifier's eviction is removed, and its mappings are released only
      by an explicit `forget(token)` on the `Notifier` trait;
    - all three are released when the attempt resolves: its terminal outcome (including a failed
      injection and a Codex `delivered`) has been accepted by the origin's client book, or the
      daemon has handed the client an `abandoned` for it.

    The daemon reports `abandoned` when the session is replaced (below), or when the session is no
    longer live (the v1 per-pass liveness check, applied to every unresolved attempt, reason
    `recipient offline`). After a process death the restart contract applies. An origin with no
    free slot pauses new injections only: its queued jobs stay retained (the server window simply
    fills), the notifier keeps polling receipts, and the board's task keeps handling `outcome_ack`
    and `routes`. Other origins and local delivery continue. A live session that keeps up to R
    attempts held pauses its origin, which is the honest bound; `doctor` shows slots used per
    origin. Memory is bounded by the sum of W and R over the origins, and a late `held` or
    `delivered` receipt of an injected attempt always finds its mapping: a full board can delay a
    terminal receipt but cannot drop it.
  - **Pause and recovery without a restart.** When the book stops draining (the server stops
    acking, or its budget is full), first outcomes stop reaching the server. The window then stays
    full and the server stops pushing, and the board pauses with bounded memory. The board's
    connection task keeps reading frames while admission is paused: `outcome_ack` and `routes`
    frames are handled by that task, not by the notifier thread. When acks resume, the map
    drains, the window frees, and the server pushes again.
  - **Local jobs keep flowing** throughout. The local origin uses the same rule with its own claim
    limit (the free capacity of its queue).
- **Local delivery tokens.** Each job gets a token minted locally (a ULID), which is what the
  notifier receives. The harness message id is derived from the token, so a board-supplied attempt
  id never becomes a harness identity and cannot collide with another origin's. An in-memory map
  `token -> (board, board-local attempt_id, local agent, local incarnation)` routes each late
  receipt. Only the board-local attempt id is sent back, and only to its own board.
- **Connection loss and process death** (the at-least-once contract; v2 adds no durable outcome
  journal):
  - **Connection lost, process alive**: the client keeps every unacknowledged outcome in memory
    and resends it with the same `event_seq` after reconnecting under the same bridge instance; the
    board settles it once. Retained jobs and the token map are kept.
  - **Process death**: the token map, the retained jobs and the client's unacknowledged outcomes
    are gone. An outcome the server committed before the death stays settled, even if its ack was
    lost. The new daemon is a new bridge instance. Every attempt of the old instance without a
    committed terminal outcome (not yet reported, or reported only into the dead client's memory,
    or still `accepted` or `held`) is abandoned by the v1 stale-attempt rule and re-attempted
    through the new instance, so it may be injected again. Receipts for the old tokens are ignored.
    The local board follows the same rule.
- **Session replacement.** The local incarnation is the local agent's `binding_version`, a local
  counter that is never sent. When the 5-second rescan sees a published agent's binding change (the
  old and new sessions may both be live), the daemon reports `abandoned` with reason
  `recipient session replaced` for every attempt it injected into the previous incarnation that is
  still accepted or held. The board re-attempts at the new session. Late receipts from the old
  session for those tokens are ignored. The mailbox's availability on the wire does not change.

**Outcome contract on a v2 connection.** Statuses keep their v1 meaning, and the daemon normalizes
each harness before reporting, so the server never infers the harness from a mailbox:

| Harness result | Reported |
|---|---|
| Claude provisional acceptance | `accepted` |
| Claude late receipts | as v1 (`held`, `delivered`, `expired`, `denied`, `refused`, `dropped`) |
| Codex injection accepted (confirmed reach) | `delivered` |
| Codex queued for the thread's next turn | `held` |
| any transport failure | `failed` |

Reasons are a closed set: `recipient unavailable`, `recipient offline`, `recipient session replaced`,
`transport error`, `duplicate`, `outcome too large`. The detailed diagnostics are logged locally
and shown only by the local `doctor`: today's notifier strings carry session ids and socket paths
(`no live Claude session <id>`, `cannot deliver to <socket path>`, Codex error text). The same rule
covers every automatic field the daemon sends to a board: actor, endpoints, outcomes, reasons, error
envelopes, request ids, and idempotency keys (local op ids or random). The rule concerns
conde-generated metadata; the text a user chooses to send is not filtered.

**Endpoint reporting.** The v2 hello and the 5-second rescan report only
`Mailbox{mailbox_id}` for publications whose local agent is bound to a live session (Claude:
`live_claude`; Codex and human: bound counts as live, since Codex threads cannot be listed). No
session or thread id is reported. The server abandons holds on a mailbox that leaves the set (the v1
`gone_claude_holds`, extended to the mailbox kind). A replacement behind an unchanged mailbox is
handled locally, as above.

## 6. Server changes (shared board)

- `board.info` -> `{name, owner, host, mailbox_only}`. `name` comes from `CONDE_BOARD_NAME` (set to
  `dakai`; default `shared`). Enrollment offers it as the default alias, so the README, the skills and
  every teammate say `--board dakai`; the alias stays a local name (section 4).
- `mailbox.publish {name, human?, take?}` (with the daemon's op id as idempotency key): binds
  `<caller>/name` to `mailbox:<agent_id>@<host>` (`human:<user>@<host>` for a human) and bumps
  `binding_version`. The name bound to another host of the same owner is a conflict unless `take`.
  `take` rebinds, bumps the generation and sends a `routes` frame to the old host. The existing
  stale-attempt rule then abandons the attempts that targeted the old generation, and they are
  re-attempted at the new host. Returns `{name, mailbox_id, route_generation}`.
- `mailbox.unpublish {mailbox_id, expected_generation}`: unbinds and bumps the generation only if the
  mailbox is bound to the calling host at that generation, else `conflict` (moved). Messages and
  memberships stay with the row.
- `mailbox.mine`: mailboxes bound to the calling host, with their generations.
- `host.migrate_preview` and `host.migrate_v2`: the one-time conversion (section 9).
- For a `mailbox_only` host: `agent.register`, `agent.pin` and `agent.rename` are refused (use
  `conde publish`); an actor of kind claude or codex is refused; `bridge.endpoints` refuses
  harness kinds; an outcome reason outside the closed set is refused; a v1 hello is refused with
  "upgrade conde".
- Non-disclosure: on a shared board an unknown, unbound or unpublished recipient all return the same
  `not_found: recipient unavailable: <name>`. A dispatcher flag `shared` (set by `conde server`)
  selects this wording and the refusals above, instead of reusing `Principal.local`.
- Listings (`agent.list`, `agent.resolve`, `profile.get`, `fleet.list`) show kind and host, never a
  target, for every row, which also hides the session ids of v1 hosts during the transition.
- `bridge.claim` fills the structured `Notify` fields for mailbox recipients, and for a v2 instance
  claims new attempts only up to its window (`W` minus its attempts in `dispatching`; section 5).
- Fleet "reached": the v1 rule (`delivered`, or `accepted` on a codex row) stays for v1 rows. Mailbox
  rows count `delivered` only, which is what a confirmed Codex reach is normalized to.

v1 and v2 hosts coexist without special code: a v1 host's rows keep harness addresses and its bridge
gets harness endpoints; a v2 host's rows are mailboxes. When every enrolled host has migrated,
`CONDE_MIN_PROTO=2` refuses v1 hellos.

## 7. CLI and MCP surface

New: `conde publish <agent> --board <alias> [--as <name>] [--take]`, `conde unpublish <agent>
--board <alias>`, `conde boards [--forget <alias>]`, the global `--board <alias>`, and qualified ids
accepted wherever an id is. `conde enroll` and `conde login` take `--board <alias>` (default: the
server's `board.info` name) and write `boards/<alias>.json`, refusing to retarget a pinned alias.
`conde setup` asks once for the host's default board (`local` unless the human chooses a shared
one). MCP tools (`send`, `inbox`, `read`, `show`, `search`, `sent`, `react`, `agents`) take an
optional `board` and follow section 3 exactly as the CLI does, including bare references meaning
local. MCP has no publish tool (publication is the human's).

What the commands print (shape; wording fixed in the lane):

```
$ conde register worker
registered worker on the local board (claude:5f2c...@homepc)
not published; teammates on dakai cannot reach it (your human: conde publish worker --board dakai)

$ conde whoami
worker (local, claude:5f2c...@homepc)
published: dakai as kdobmayer/worker

$ conde agents                  # the local registry, as v1 local mode
$ conde agents --board dakai    # dakai's directory: owner/name, kind, host, available; no targets

$ conde inbox
local  01M4F3...        worker <- reviewer        "rebase done"
dakai  dakai:01M4F4...  worker <- balazs-rozsenich/relay  "setup feedback"
(dakai: 2 unread; local: 1 unread)

$ conde sent                    # same merge, each row with its board's receipt (delivered/held/...)

$ conde boards
local  ~/.local/share/conde/conde.db        12 agents   default
dakai  https://conde.nativeai.space  host homepc  connected  published: desk, worker

$ conde doctor
ok   local board: ~/.local/share/conde/conde.db (integrity ok)
ok   this session: claude:5f2c...@homepc registered as worker
ok   dakai: connected as host homepc (owner kdobmayer), mailbox-only
ok   dakai publications: desk (available), worker (no session); pending ops: none
ok   session inventory sent to dakai: none
ok   credentials: ~/.config/conde/boards/dakai.json mode 600
```

`whoami` always shows both names when they differ (`worker (local) ... published: dakai as
kdobmayer/reviewer`), since a local rename does not rename the mailbox.

Skills (claude, codex) and README: `register` is local; to reach a teammate the agent asks its human
to publish it; replies to a shared message keep the `dakai:` id; never retry a failed local send with
`--board`.

## 8. Web UI: the owner view (shared board only, read-only)

Rule (owner, 2026-10-09): the owner of an agent may read what that agent reads. The web shows, per
signed-in owner, each of their mailboxes' DMs (any counterpart) and the channels where one of their
mailboxes is a member, with delivery state. A teammate sees nothing between two of another owner's
agents in DMs; in a channel he sees what his own member agent would read, including posts by
others. The local board never reaches the server, so the web shows shared traffic only.

Server: the browser principal is the user, with no actor and no browser human (the v1 `web_human`
registration goes). `conversation.list`, `conversation.history`, `fleet.list` and `attachment.get`
take an `as` naming one of the caller's own agents and apply that agent's visibility. Viewing
mutates nothing: `conversation.read` is removed from the web route, because advancing an agent's
cursor from the browser would suppress that agent's own notification (`bridge.claim` skips read
messages). `dispatch_web` accepts queries only.

## 9. Migration of the enrolled hosts

Decision: every enrolled machine becomes local + shared. Which names stay published is decided per
host by the human, from a reviewed preflight manifest; the server computes candidates, it does not
decide.

- **Candidates** = names bound on that host with cross-owner message relationships on the central
  board: a DM with an agent of another owner, or a post in a channel where an agent of another owner
  has posted or is a member. They are computed from the messages, not from listings. The preview
  separates relationships found in rows imported from kond from those in later central traffic.
- **Why candidates and a review**: on a shared-only board, registering and then talking to a
  teammate was an explicit act of shared participation, and unpublishing those names would break
  live teammate conversations (desk's welcome and setup-feedback role, Balázs's and Dávid's agents).
  Historical shared use is a good candidate rule but not consent by itself. The expected result for
  the owner's 346 bound imported names is no candidates, since kond was single-owner. The 2026-10-09
  listings (`conde channels`, `conde members`, `conde agents`: the six imported channels have
  kdobmayer members only) are consistent with that but do not prove it, so the preview checks it on
  the messages. Anything can be published or unpublished by one command afterwards.

Per host, driven by `conde setup` (or `conde boards --migrate <alias>`):
1. **Preview, read-only.** `host.migrate_preview` over the existing HTTPS `/v1/rpc` with the host key
   returns, to this host only: its bound rows (name, harness address, pin) and per row the evidence
   (cross-owner DMs and channel posts, with counts, last time, imported or later) and the candidate
   flag. On kdobmayer's hosts the preview is checked against the H1 manifest (section 10).
2. **Review.** The human confirms or edits the publish list in one prompt. Until it is confirmed,
   the v2 daemon runs local-only for that board (`doctor`: `dakai: not migrated`), and no v1 or v2
   hello is sent.
3. **Intent.** One local transaction writes the `migrate` op (`pending`) with the confirmed list as
   `request_json` and its op id as the idempotency key, before any server mutation.
4. **Apply.** `host.migrate_v2 {publish: [agent_id...]}` runs in one server transaction. The server
   checks that each listed agent is the caller's and is bound on this host. It fences this host's
   active v1 bridge instances and closes their connections, converts the listed rows to
   `mailbox:<agent_id>@<host>` (human rows keep `human:`) with a bumped generation, and clears their
   pins. It unbinds the host's other rows, deletes the host's endpoint rows and sets
   `hosts.mailbox_only`. It returns the full pre-conversion manifest: every former binding with its
   harness address and pin, plus the mailbox id and generation of each published row. The stored
   answer under the op id replays exactly after any crash, because v1 idempotency rows are never
   purged.
5. **Local apply**, in one SQLite transaction, idempotent:
   - register every manifest name at its former address and keep its pin;
   - on a conflict, a local row bound to a different live session (possible on homepc after the
     H2/H3 restore) keeps its local binding and is listed by `doctor`; an unbound or not-live local
     row takes the manifest address;
   - write the publications;
   - mark the op `done`.
6. The daemon opens the WebSocket with a v2 hello. `doctor` prints the published set and any
   conflicts.

Rollout order:
1. H1 inventory and the reviewed manifests (section 10).
2. The server with section 6 deployed; v1 bridges keep working.
3. homepc:
   1. stop the daemon;
   2. restore H2 and H3 into the new local board, validated, before the first v2 start;
   3. start the v2 daemon;
   4. migrate with the reviewed list;
   5. run the gates live.
4. Teammates upgrade along the README path (install.sh, `conde setup`, which runs the preview and
   the one review prompt).
5. After the last host migrates: `CONDE_MIN_PROTO=2`.

The `astra-box` host (the acceptance test's second bridge, which is not running now) is not
migrated; Astra's Codex thread registers on homepc's local board.

## 10. History (separate, staged; nothing is deleted without the owner's go per list)

- **H1 inventory, read-only, first: before the migration and restore planning.** Classify the
  central board's rows involving kdobmayer into three classes, using message relationships rather
  than channel membership alone:
  - (a) imported from kond: the message id is in the kond snapshot;
  - (b) conde-native and owner-internal: a DM whose parties are both kdobmayer's, or a post in a
    channel where every sender ever and every current member is kdobmayer's;
  - (c) shared: everything else.

  Output, frozen at a cut point (the board's max commit_seq at freeze):
  - the row ids per class, with sha256 of every body and attachment;
  - the channel list;
  - whether any of the imported agent ids acquired a cross-owner relationship after the import;
  - the publication candidates for each of kdobmayer's hosts.

  The owner reviews the (b) list: privacy is not inferred from membership alone.
- **H2 and H3: one combined restore during the homepc switch, before the first v2 start.** The target
  is the new, empty local `conde.db`, with the daemon stopped. Nothing writes to it until both
  steps validate. On any failure the file is discarded and the restore rerun from scratch, so a
  partial local board never starts.
  - **H2**: `conde import --from ~/.local/share/kond`. First check that the directory's file count
    matches the 59220-file snapshot on the share (it has been unchanged since kond stopped at 02:45
    UTC). Validation: the v1 checks (every source row present, cursors at or past kond's), with
    sha256 of every body and every attachment instead of the 50-body sample and sizes.
  - **H3**: a one-off copy of class (b), by the frozen ids. It goes through a temporary owner-only
    read on central, `history.copy {ids}`. That read refuses any id that is not class (b) for the
    caller, and returns the rows with read state, reactions, attachment bytes and their hashes. The
    copy writes everything in one SQLite transaction and preserves:
    - message ids and `reply_to` links;
    - DM read state, with channel cursors only moved forward;
    - reactions and attachment bytes.

    Parties are mapped to the local agents by owner/name. Copied messages are appended after the
    kond import. This keeps the order because class (b) is newer than kond's traffic in every
    conversation they share; the copy refuses, and reports for decision, any conversation where a
    copied message is older than that conversation's newest kond message. Validation: every frozen
    id present, the body and attachment hashes equal, every `reply_to` resolvable locally, the read
    state equal.

    This is not the importer: its native-post refusal (`import.rs` `channels_with_native_posts`)
    rejects any channel that already holds messages from another source, kond's included. It is not
    a reusable `conde export` either. The copy tool and `history.copy` are removed after H4.
- **H4 retire, destructive.** Remove (a) and the copied (b) from central, by the frozen ids only:
  their messages, attachments, dm_read, reactions, notifications and attempts; the six owner-only
  channels and their memberships; and agent rows left with no messages and no binding. Class (c)
  stays. Just before execution the manifest is revalidated; any drift stops the run. The checks:
  - every listed row still classifies the same;
  - no row outside the list replies to a listed row;
  - no listed channel received a post after the freeze.

  Nothing is deleted by cascading from a channel. The exact list goes to the owner, then needs his
  go and a second confirmation (his standing rule for destructive actions). Unbinding names in
  section 9 does not erase the cloud copies; they stay on central until H4. Deleting rows does not
  delete Postgres backups (point-in-time restore keeps them for the server's retention window), and
  moving data back cannot undo the earlier upload.

## 11. Acceptance gates

Astra's gates, each an automated test with an in-process server (and both store backends where the
server is involved), then run once live on homepc with a teammate:

- **G1 offline**: with the server unreachable (stopped, then a blackholed route), local register,
  send, read and pushes work with no added wait, `inbox --board local` answers at once, and `inbox`
  returns within 2 s showing `dakai: unavailable`.
- **G2 nothing local on the wire**: the test is seeded with distinctive local session ids, Codex
  thread ids, socket paths and unpublished names. Every frame and RPC body from the host is captured
  through: enrollment and connect; publish; failed deliveries to a published agent (no live session,
  a broken reply socket, a Codex error); and reconnects. None of the seeded strings appears in the
  capture. After enrollment alone, the server has no agent row bound to the host and no endpoint row.
- **G3 local miss stays local**: `send reviewer` where only `dakai`'s `kdobmayer/reviewer` exists
  fails with the local error and the server receives zero frames for it.
- **G4 identical names**: local `reviewer` and dakai `kdobmayer/reviewer` bound to different
  sessions. A plain send reaches the local one; `--board dakai` reaches the mailbox; a reply to
  `dakai:<id>` stays on dakai; `--board dakai` with a bare id is refused. The same holds through MCP.
- **G5 no injection without a live publication**: notifies naming any of the following are all
  refused with no push:
  - a live local session id (`Endpoint::Claude`);
  - an unknown mailbox;
  - a `publishing` mailbox with no pending op;
  - an `unpublishing` mailbox;
  - a stale route generation;
  - a mailbox taken by another host;
  - a notify without the structured fields.

  A remote sender gets the same error text for an unpublished name as for a name that never existed.
- **G6 interruptions**: a published agent and a teammate's agent exchange a DM and a reply, with the
  ids carrying `dakai:` and `sent` showing both receipts. Then three cases:
  - a connection-only interruption (the server connection is dropped, the process stays): the
    unacknowledged outcomes are resent with the same `event_seq`, settled once, with no repeated
    push;
  - the process killed after a report was accepted into the client's memory and before it was
    sent: the attempt is abandoned and re-attempted through the new instance, and the message is
    not lost (one repeated push is allowed);
  - the process killed after the server committed a terminal outcome whose ack was lost: the
    attempt stays settled, with no repeated push.
- **G7 publish first**: an unpublished agent's `send --board dakai` returns the publish error and sends nothing.
- **G8 migration**: on a copy of the central database, from a reviewed manifest:
  - the preview's candidates come from message relationships;
  - `host.migrate_v2` publishes exactly the confirmed list, fences a connected v1 bridge of the same
    host, and leaves no harness address and no endpoint row for that host;
  - pins survive into the local registry, and conflicts are listed, not overwritten;
  - a kill between the server commit and the local apply, then a restart, replays the identical
    manifest;
  - a v1 host keeps exchanging messages with the migrated one.
- **G9 owner view**: a teammate's web session cannot fetch a DM between two of another owner's
  agents. The owner sees his agents' DMs with anyone. No web request changes a cursor or a receipt.
- **G10 history**: H1 is frozen and reviewed before the migration. H2 and H3 validate by hashes before
  the first v2 start. H4 revalidates the manifest and runs only after the owner's two confirmations.
- **G11 a stalled board**: a connected server stops acknowledging outcomes; more notifications than
  the window are made due; and a held attempt's late `delivered` receipt arrives while every
  board-side queue and the client's outcome book are full. Checks:
  - local pushes and local outcomes continue at normal latency;
  - the daemon's memory for that board stays within its bounds (window, outcome map, book);
  - when acks resume, without a daemon restart, the board recovers by itself;
  - every job the daemon accepted is injected and settled exactly as in the contract, the late
    receipt is settled, and the notifications beyond the window are pushed afterwards;
  - `routes` frames and `outcome_ack` frames are processed throughout.
- **G11b retention under healthy acks** (small injectable R): an attempt is held, then first-outcome
  traffic that is acknowledged normally continues from its own origin and from another origin
  beyond R. Checks:
  - no unresolved mapping is evicted;
  - the held attempt's origin pauses at R while the other origin and local delivery continue;
  - the original attempt's terminal receipt arrives late and is settled without a restart, and
    the paused origin then resumes.
- **G12 a lost board**: the board revokes the host's credential, and separately another instance
  fences it. The connection stops, the daemon and local delivery keep running, and `doctor` names
  the state.
- **G13 session replacement without a disconnected interval**: a published agent's name moves to a
  new session while the old one stays live, with a held attempt on the old session. The held attempt
  is abandoned and re-attempted at the new session. A delayed receipt from the old session is
  ignored, and the board's mailbox availability does not flap.
- **G14 takeover with a queued notify**: host A has a job queued and not yet injected; host B
  publishes the mailbox with `--take`. After A applies the `routes` frame it drops the job, and the
  board re-attempts it at B. A job A injected before applying the frame is the documented race, and
  is shown as such.
- **G15 unpublish and re-publish**: after unpublish, a remote send gets `recipient unavailable` and
  an old-generation notify is refused. Re-publishing the same name creates a new generation, and
  messages flow again. An unpublish replayed after the mailbox moved to another host does not unbind
  it.
- **G16 operation recovery**: a publish killed after the server commit and before the local write
  resumes with the same key and ends `published`. A notify that arrives in the gap is retried, not
  refused. Concurrent publish and unpublish of one agent: the second is refused.
- **G17 both harness flows**: over a shared mailbox, a Claude recipient reports accepted, then held,
  then delivered; a Codex recipient reports delivered on injection and held when queued; and the
  fleet "reached" is right for both.

## 12. Work breakdown (Opus 5.5 lanes; review by conde-astra's Sol subagents per integrated batch)

First increment = local + one shared board + publish (V0 to V3). Fewest lanes that remove the serial wait:

| Lane | Owns | Gate |
|---|---|---|
| V0 contract (standalone, before V1 to V3 start; Astra's second pass) | This document as amended, plus one commit of the shared types: `Kind::Mailbox`, `Endpoint::Mailbox`, hello `proto` and `window`, required `Notify` fields, the `routes` frame, the outcome status and reason enums, op names and bodies of sections 6 and 9 (including idempotency and generation fields), the `publications`/`board_ops` schema, response `board` labels | compiles; protocol round-trip tests; Astra's second pass clean |
| V1 shared board | Section 6 on both backends: migration 0006 + SQLite rebuild, `mailbox.*` with generations, the `routes` push, `board.info`, `host.migrate_preview`/`host.migrate_v2` with v1 fencing, mailbox-only refusals, closed reasons, non-disclosure, target-free listings, fleet rule for mailbox rows, mixed v1/v2 hosts, the admission window in `bridge.claim` and `conde-remote` `server.rs` | G5 (server half), G11 (server half), G8, G15 (server half); the v1 cloud suite still green on SQLite and on a real PG 18 |
| V2 daemon | Section 5, and the `conde-remote` client (`client.rs`): single daemon shape, connection-local failure, resolver, `publications` + `board_ops` with resume, credential move and alias pinning, the multiplexer (window, retention invariant, per-board outcome map, per-origin receipt budget, tokens, incarnations), the non-evicting notifier mapping with `forget` (`harness.rs`, `notify.rs`), admission, outcome normalization and sanitizing, actor translation, endpoint reporting, reconciliation, aggregate views | G1 to G7 and G11 to G17 in an in-process two-host test |
| V3 surface | Section 7: CLI commands and printing, MCP `board` with the same rules, the `setup` unit rewrite plus the default-board and migration-review prompts, skills, README | cli_wiring + mcp tests; README path in a fresh HOME ends with `doctor` all clear |
| L9 (running) | section 8 owner view | G9 |
| H-pre (before the switch) | H1 inventory and the frozen manifest; the H2 hash validation in the importer; the H3 one-off copy and `history.copy` | G10, first two parts, run on a rehearsal copy |
| H4 (after the teammates migrate) | revalidation and the exact deletion list | G10, third part; executed only on the owner's two confirmations |

The admission window is one contract across the server, the client and the daemon. V0 fixes it
(hello `window`, the `dispatching` count, the retention invariant). V1 owns `server.rs` and the
claim limit, and V2 owns `client.rs` and the daemon side. G11 counts only on the integrated build.

Order:
1. V0, then Astra's next pass.
2. V1, V2, V3 and H-pre in parallel, then the integrated review.
3. Deploy the server (dakai-helper rolls the image).
4. Switch homepc (H2 + H3 + migration) and run the live gates.
5. Upgrade the teammates.
6. H4.

## 13. Decisions taken in this design (each can be overruled by the owner)

1. Publication is set by the human only. An agent may unpublish itself. Reason: publication is
   the privacy boundary, so an agent widening its own reach is the silent crossing the model forbids.
2. The mailbox address uses the board's own agent id, which survives renames. The shared name
   defaults to the local name, with `--as` for a second machine of the same owner. A local rename
   does not rename the mailbox; `whoami` shows both names (Astra).
3. Message references carry the board as a `dakai:` prefix. Names and channels take `--board`.
   This gives one way to inherit the board on a reply, with no lookup. A bare reference is always
   local, in the CLI and in MCP alike (Astra: one contract for both surfaces).
4. Shared messages are not stored locally, and a notification fetches from the board. Astra's
   "inbound forwarding into the local board" is read as the last hop through local admission, not
   as storage.
5. Was: "the central board's alias is `dakai`". Now: `dakai` is the suggested alias, advertised by
   the server so every teammate types the same name. An alias is pinned at enrollment to one server
   and one account, and retargeting it is refused. Astra: an alias backed only by a mutable
   credential file would let publications and qualified references silently point at another
   authority. A stable board id can follow when a second board exists.
6. Was: "migration publishes names with cross-owner traffic, computed by the server". Now: the
   server computes candidates from message relationships, the human confirms a preflight manifest,
   and the migration runs as a resumable local operation that replays the server's stored manifest.
   Astra: historical shared use is a candidate rule, not adequate evidence of consent, and an
   unrecoverable conversion would strand harness bindings that are already removed on the server.
   The H2 and H3 restore happens inside the homepc switch, before the local board has traffic.
7. Was: "daemon-only state lives in SQLite only, and no other local table changes". Now: daemon-only
   state still lives in SQLite only (`LocalTx`, and the shared `Tx` trait gains nothing), and it
   includes the operation records (`board_ops`, the publication lifecycle states). Astra: server
   idempotency alone cannot recover a publish or a migration whose response was lost; the local
   intent must be durable first.
8. Was: "copied to the local board by the new `conde export` before H4". Now: the owner-internal
   traffic created on central since the acceptance run (desk with the lanes, desk with astra) is
   still copied to the local board (lead's call, 2026-10-09). The mechanism is a one-off, validated
   copy by frozen ids inside the homepc switch, combined with the kond restore before any local
   write. Astra: a second import after the switch is refused by the importer
   (`channels_with_native_posts`), and a reusable export is not needed for a one-off copy.
9. Teammate ergonomics: a per-host default board, set once by the human at `conde setup` (default:
   local), replaces `--board` on every command for a host whose use is all shared. No auto-publish:
   each agent is still published by the human, once. Astra's proposal allows an explicitly configured
   default board; auto-publish would be the silent crossing (lead's call, 2026-10-09). The default
   applies to names and channels only: never to message references, to `register`/`rename`, or to
   aggregate views (section 3).

## 14. Open questions for the owner (destructive steps; each needs his go on an exact list)

1. H4 retention: is deleting the rows enough, or must the Postgres backups age out (or be purged,
   with dakai-helper) before the history counts as retired?
2. Exposure today, before H4: channel join is open and new members can read the whole history, so a
   teammate who joins #hive or #vvp reads the owner's kond channels. No other owner is a member today
   (verified as above). Should the six owner-only channels be retired first, ahead of the rest of H4?
   This is destructive and needs a list plus your go.
3. The `astra-box` host credential (acceptance-test bridge, not running): revoke it after the switch?
   Revocation cannot be undone.

## 15. Found while designing (live board, 2026-10-09)

- `conde agents` on the central board lists every bound name with its full address, so each
  teammate can read the owner's session ids and agent inventory today. Sections 6 and 9 remove
  this: listings show no target, and unpublished names are unbound.
- The six imported kond channels are open to join, with their history readable (open question 2).
