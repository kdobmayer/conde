# Design v2 second pass — 312c92f

Verdict: not ready to start V1–V3 yet. Two delivery-contract gaps remain in V0. This is a solo design review, checked against the transport source at the same commit; no implementation changes or runtime tests were made.

The seven first-pass amendments are substantially incorporated. Durable publication/migration recovery, route generations, closed metadata, local tokens, harness normalization and the frozen history workflow now have useful contracts and gates. The explicit default-board setting is acceptable. H4's two confirmations are accepted as the owner's standing requirement.

## P1 — Specify lossless bounded admission before relying on re-offer

`docs/DESIGN-v2.md:249–251` says that when a board's outcome queue fills, its jobs remain open on the server and are offered again later. That does not follow from the current transport:

- `crates/conde-remote/src/server.rs:987–992` remembers each pending attempt in a per-connection `sent` set and does not push it again while it remains pending.
- `crates/conde-remote/src/client.rs:792` marks an attempt seen before sending it to the notification callback; `Seen` survives reconnects (`:555`). There is no callback admission acknowledgement that can undo this suppression.
- The callback's incoming channel is unbounded (`:268`). Blocking that callback on a bounded daemon queue moves the backlog into this channel. Dropping a notification instead can strand the open attempt: reconnect alone does not clear client deduplication.

The new multiplexer needs an explicit transport admission/recovery contract, not just bounded queues after the callback. Choose and specify the mechanism in V0 (for example, admission credits with bounded retained jobs, or a retryable rejection that reliably creates another delivery opportunity). Define when an attempt becomes admitted/seen, who retains it until then, and how recovery occurs without process restart. Keep outcome acknowledgements and route revocations serviceable while admission is paused.

Also account for late receipts from jobs already injected: stopping new jobs does not stop their `held`/`delivered` outcomes. Reserve bounded capacity for these outstanding attempts, or specify another bounded retention scheme, so a full board queue cannot drop a terminal receipt or block the shared notifier.

Extend G11 to fill every relevant queue, deliver a late terminal receipt while full, recover the board without restarting the daemon, and assert that every accepted job/receipt survives, memory remains bounded, and local delivery continues. This affects both server/client transport and daemon ownership, so settle it before splitting V1/V2.

## P2 — Separate reconnect replay from process-crash recovery

`docs/DESIGN-v2.md:258–262` explicitly discards the token map on restart, then promises that a crash after reporting but before acknowledgement resends the outcome with the same `event_seq`. No durable outcome journal or sequence recovery is specified. The reused v1 client retains outcomes only in memory (`crates/conde-remote/src/client.rs:149–154, 255–261, 389–398`); its retransmission contract covers a live client reconnecting, not a killed process.

The smallest fix is to state the actual at-least-once contract:

- Connection loss with the process alive: retain and resend the same outcome/event sequence.
- Process death: a terminal outcome committed by the server remains settled; an attempt without a committed terminal outcome is recovered through the new bridge instance and may inject again. Old local receipt tokens are ignored as already specified.

Split G6 accordingly. Distinguish a report accepted into the client's memory, a server-committed terminal outcome whose ack was lost, and a connection-only interruption. Do not claim process-crash event-sequence replay unless V0 deliberately adds durable outcomes and defines how replay interacts with bridge-instance fencing.

## Rejected candidates / scope

- Takeover cannot recall an already injected push: the revised design states the observation race honestly; this is not a remaining blocker.
- Persistent publication and migration intents address the server-commit/local-apply crash window; no additional registry rollback protocol is required for the stated workflow.
- Historical import needs a dedicated adapter: the revision now says so and puts frozen hashes and validation before the local switch.
- H4 confirmation count is an owner decision, not a technical finding.

What would make me reject this? A V0 contract that can lose a queued remote delivery or late receipt under pressure, stall local work behind that pressure, or promise recovery state that exists only in the crashed process. Close these two items and the design can proceed to implementation review.
