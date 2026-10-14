# Design v2 third pass — adf7456

Verdict: one remaining P1 in the bounded receipt-retention contract; not ready to build yet. The reconnect/process-death correction closes P2. The server window and explicit ownership resolve the previous transport handoff ambiguity, but the claimed bound on outstanding receipt state is not lossless yet.

## P1 — An evicting receipt cache is not reserved capacity for unsettled attempts

`docs/DESIGN-v2.md:259–264` frees window capacity on the first outcome, including `accepted` and `held`. Lines 272–279 then derive a lossless late-receipt bound from the notifier's `REMEMBERED` cap.

At this commit, `crates/conde/src/harness.rs:55–62` evicts the oldest message-to-attempt mapping once `REMEMBERED` is exceeded, regardless of whether that attempt has settled. `:129–131` ignores a subsequent receipt whose mapping has been evicted. This is one global notifier cache, so traffic from another origin can cause the eviction too.

Concrete sequence: inject an attempt, commit and acknowledge `held` (freeing its dispatch window slot), then continue injecting and acknowledging first outcomes. After enough injections, the first held attempt's harness mapping is evicted. Its eventual `delivered` receipt is discarded before the per-board outcome map can reserve or use any slot. A larger daemon token map cannot repair that lost harness lookup. Keeping every unresolved mapping instead removes the claimed memory bound, because the dispatch window does not count accepted/held attempts.

Required contract adjustment: bound outstanding receipt obligations per origin and retain their harness and daemon mappings without eviction until terminal resolution. Reserve capacity before injection; when an origin has exhausted that capacity, pause that origin's injections while continuing to poll receipts and handle control frames. Keep local capacity independent so a shared board cannot exhaust it. The existing server dispatch window can remain, with its queued jobs retained while injection capacity is exhausted. Define when the reservation is released and ensure a pending terminal outcome remains retained until the client has accepted responsibility for it. An alternative bounded design is fine, but silently evicting an unsettled mapping is not.

Extend G11 with a small injectable retention cap: hold one attempt, continue first-outcome-acknowledged traffic from its origin and another origin beyond the cap, then deliver the original terminal receipt. Assert no unsettled mapping is evicted, the capped origin pauses at its bound, other-origin/local work continues, and the terminal receipt eventually settles without restarting. The current stalled-ack case alone does not exercise this healthy-first-ack retention failure.

The terminal-over-nonterminal coalescing rule is accepted. No new durable outcome journal is needed. This is a solo source-checked design review; no implementation edits or runtime tests were made.

What would make me reject this? Claiming lossless bounded late receipts while relying on a cache that forgets the unsettled receipt identities.
