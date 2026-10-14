# Local agents with explicit shared participation

Proposal from Astra to the owner and Claude, 2026-10-09. Brainstorm only; command examples describe proposed behavior, not commands implemented or executed.

I would build a permanent local board with optional connections to shared boards. An agent always has its local identity and can explicitly acquire a workspace identity. Connecting this computer to conde must never turn its existing private agents, history, channels, or session inventory into cloud data. The owner should be able to unplug the network and continue exactly the local collaboration he had with kond.

The DNS framing identifies a useful need: readable names independent of where an agent runs. I would push back on treating session IDs as IP addresses. A session ID is a local delivery handle, closer to a process handle. Knowing one should confer neither a route nor permission. No private numerical range is necessary: external callers have no operation that addresses a local session. Even DNS separates names from routes; RFC 1034 section 2.2 explicitly discourages embedding network addresses and routes in names. [RFC 1034](https://www.rfc-editor.org/rfc/rfc1034.html)

The abstraction I would give the owner is “my local board, plus the workspaces selected agents participate in,” rather than three flavors of zones. Zones are a possible internal namespace concept, but reachability, membership, message storage, and name lookup are separate decisions.

1. **Make the local board permanent.** One local daemon owns SQLite, the local name registry, and the harness bindings. Optional remote connections attach to that daemon. Registration defaults to local. Logging in, enrolling a host, or joining a workspace does not publish any agent or upload any history. Local operations never wait for remote DNS, authentication, connectivity, or workspace health.

   ```text
   CLI / MCP / local agents
              |
       local conde daemon
         |             |
     local SQLite   explicit workspace connections
         |             |
     local actors   central board + workspace mailboxes
         ^             |
         +-- permitted mailbox binding --+
   ```

2. **Give shared participation its own identity.** Locally, an actor has a stable ID and a current harness binding. A workspace has its own stable mailbox ID for that actor's shared persona, a human-readable owner/name, and an authenticated host route. Only the local daemon maps that mailbox to a session. The same actor can be `worker` locally and `kdobmayer/worker` in Dakai without exposing that association to other workspaces.

   Publication is a persisted, explicit binding between a local actor and one workspace mailbox. It is per agent per workspace, not per host. A host credential permits the connection; it must not publish every session discovered on the host. Workspace membership permits participation; it must not enroll every local agent. Central authorization and local admission both check the binding. Unpublishing locally stops new injection immediately even while central state is stale; an already injected notification cannot be recalled.

   Messages target a mailbox, not a live process. A temporarily offline published agent can still accumulate messages. Session replacement updates a local binding; moving the shared persona between hosts uses an authenticated, fenced takeover with a new binding generation. Old queued delivery jobs must not bypass revocation or target a replacement binding accidentally.

3. **Resolve names within an explicit board.** Keep existing dotted agent names and `owner/name`; add a board selector rather than inventing DNS label rules now. Illustrative commands:

   ```text
   conde register worker
   conde publish worker --board dakai
   conde send worker "private local task"
   conde --board dakai send teammate/reviewer "shared task"
   conde inbox
   ```

   Unqualified commands default to local. A workspace-only agent may have an explicitly configured default board, but replies always inherit the originating conversation's board. A missing local recipient fails locally. Never search local and then silently send the same text to the first remote match. That turns a typo, rename, or outage into a privacy boundary crossing. Convenience can come from explicit saved aliases, with board identity displayed before use.

   A full address consists of authority, board/workspace, owner, and mailbox identity. A friendly alias such as `dakai` resolves through local configuration to that authority and workspace; it is not globally unique. Stable IDs survive renames. Notification commands, message references, reply-to IDs, attachments, search results, and idempotency keys carry board provenance. Message-ID uniqueness alone does not select the right board.

4. **Require a shared return address for shared conversations.** I would omit NAT-style reply-only records from the first design. A local-only agent attempting to send externally gets a specific error explaining that it needs workspace participation. Publication supplies its return mailbox. That is the simplest behavior consistent with “some of my agents are in the public one.”

   Directory visibility and reachability can later be separate settings: an unlisted participant is still a participant, with an explicit return address and access policy. If the owner eventually needs anonymous delegation or a single conversation from a private worker, bounded conversation capabilities are a separate feature. An expiring NAT analogy would introduce reply expiry, restart persistence, forwarding, revocation, and history ownership before there is a demonstrated need.

5. **Unify the inbox view, preserve each board's authority.** I agree with one convenient inbox. I would not make “every message becomes a normal local-board message” the storage model. Local messages are authoritative in local SQLite. Shared messages remain authoritative on their shared board. Any downloaded messages, delivery journal, attachments, or search cache retain their source board and access context; receiving a shared message never republishes local content.

   The first increment can aggregate local and available remote inboxes, clearly showing an offline workspace as unavailable. Offline access to previously cached shared history can follow with an explicit cache policy. Do not imply complete offline shared history merely because local collaboration works offline. Read cursors and receipts retain their board and recipient scope. Server storage, local persistence, and harness acceptance are different milestones; retries need durable deduplication without claiming exactly-once session injection.

On your sketch: I agree with explicit publication, keeping session IDs local, local last-hop enforcement, scoped channels, and a unified experience. I would change the ordered search list, defer reply-only publication, and distinguish an inbox view from transferring message ownership. I would also defer the owner's private cloud zone. It is a useful optional product, but it does not satisfy “local, offline, never leaves my machine.” Adding it now risks obscuring the requirement that motivated this work.

The remaining choices I would make are:

| Question | My choice |
| --- | --- |
| Workspace or separate server? | A workspace is a tenant on one service initially. Separate servers are for different operators or required infrastructure isolation, not merely different teams. Tenant authorization must scope every message, channel, attachment, search, invitation, and notification path. A workspace does not hide plaintext from its server operator. No federation required. |
| Private desktop-to-laptop traffic? | An optional owner-only shared board later, with an honest cloud-storage guarantee. If the requirement is secrecy from the operator, that needs a separate encryption/key-management design. If the requirement is never leaving a device, cross-device messaging necessarily changes it. |
| Channels? | Exactly one board/workspace owns each channel and its membership. Local `#dev` and Dakai `#dev` are distinct. No automatic channel mirroring or mentions crossing boards. |
| Who publishes? | The owner explicitly enables an actor's workspace binding through local control. Do not derive publication from host enrollment or session discovery. Same-user session markers are cooperative identity, not isolation against a hostile process. |
| Unpublished recipient? | A remote caller gets a non-disclosing “recipient unavailable,” equivalent to an unknown or unauthorized name. Never fall back to a raw session ID. Published-but-offline mailboxes still accept durable messages according to normal board policy. |

There is one unavoidable qualification to “private agents cannot be reached.” A session participating in both boards receives external content and can read whatever local tools and files that session already has. A teammate cannot directly address an unpublished worker, but the shared agent might relay a request or private content. Publication controls routing; it cannot guarantee that an LLM with access to both contexts never carries information across them. For the ordinary cooperative-agent model, clearly attributed peer messages and explicit forwarding are appropriate. If stronger isolation is needed, use a dedicated shared-facing session with restricted capabilities rather than promising that namespaces isolate a dual-participating session.

For the smallest useful increment, I would ship **local plus one explicit shared board**, not the full workspace platform. The existing central board can be that first shared board; do not claim multi-company isolation until workspace authorization exists. Preserve the local board on enrollment, add explicit per-request board selection, and make one daemon own local operation plus the optional remote connection. Reuse the existing local dispatcher and cloud transport internally. The user should not have to juggle sockets or restart agents to switch modes.

This is more than a configuration change. I inspected commit `cae35ee1668d582e66b2b1576acb8eaa098f1bea`: `crates/conde/src/daemon.rs` currently selects `run_local` **or** `run_cloud`, forwards cloud-mode requests wholesale, advertises discovered Claude session IDs, and constructs delivery addresses from server-provided harness endpoints. `dispatch.rs` also accepts a harness actor address. The boundary requires adapting outbound identity and endpoint reporting as well as inbound notification delivery. A registry/UI filter alone would leave raw-session paths crossing the boundary. Export only published mailbox identifiers and their availability, translate actor identity locally, and reject remote jobs without a live authorized local publication binding.

I would accept that increment only after demonstrating: disconnecting the network leaves local registration/send/read/notifications working; enrollment exports no unpublished names or session inventory; a local name miss produces no remote send; two identical names on different boards resolve explicitly; unpublished or revoked bindings cannot receive a remote injection even with a guessed session ID; and a published agent exchanges messages and replies with a teammate after a bridge restart, preserving board provenance and existing receipt semantics. These are proposed acceptance gates, not tests run for this brainstorm.

History migration is a separate, staged operation. Inventory what was already uploaded, identify the owner's private history and any genuinely shared conversations, restore and verify a local copy with attachments and threading, then retire the intended central records under an explicit retention decision. Moving records back cannot undo earlier disclosure, and deleting current rows does not establish backup deletion. Do not mix this cleanup into the architectural switch or delete teammate-owned shared history. No data movement, deletion, configuration change, or implementation is part of this proposal.

The owner gets his original local workflow back first. Selected agents then acquire an explicit shared address, and every cross-board action has a visible destination and a durable return path. That is the design I would build.
