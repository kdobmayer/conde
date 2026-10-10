# Remote MCP security review (2026-10-10)

Scope: the remote MCP endpoint as merged (82a4001, 0616fe4, afc50b3): the OAuth authorization
server, client ID metadata documents, `POST /mcp`, the human write path and the owner read path,
the keys page's connected apps, the limits. Reviewer: Opus 5.5 (fallback; Astra's pass did not
run). Result: no P1, one P2 (fixed), nine P3 (open, NEXT.md §I).

## P2, fixed

The per-IP bucket of the OAuth endpoints keys on the peer address unless the peer is internal,
then on the last `X-Forwarded-For` entry. `internal()` (crates/conde-remote/src/web/ratelimit.rs)
did not count the carrier-grade NAT range 100.64.0.0/10, from which the Container Apps ingress may
connect (workload-profile environments reserve 100.100.0.0/17). The ingress address would then be
the key, so every user shared one 60/min bucket per replica and one client could block every
sign-in and refresh. Fixed: 100.64.0.0/10 counts as internal (IPv4 only), with its unit case; every
OAuth 429 logs the derived client IP at info, so the live check can tell which address class the
ingress uses.

## P3, open

2. Strict refresh-reuse revocation: a legitimate parallel refresh revokes the grant. If the live
   check shows Claude refreshing in parallel, add a short grace window (consumed less than 10 s
   ago: `invalid_grant` without revoking).
3. Client metadata trust is per host, not per URL (cimd.rs:206-233): pin the exact client_id URLs
   once the hosted one is seen in the log.
4. Old tokens are purged only on consent (oauth.rs:313): purge on refresh or on a timer.
5. Grants per user are unbounded: cap live grants per user and client.
6. The root `/.well-known/oauth-protected-resource` returns `resource=<origin>/mcp`; RFC 9728 §3.3
   expects the origin there: serve it only for the origin, or drop it.
7. No RFC 7009 revocation endpoint: a connector removed in the client stays listed on the keys
   page until it expires.
8. A second use of an authorization code does not revoke the tokens issued from it.
9. `search` scans the whole visible history on a pattern that never matches, limited only by the
   120/min bucket.
10. The 2048-character limit on the sign-in `next` path (auth.rs:320) can silently drop a long
    authorize request (the browser lands on /keys instead of consent).

## Residual risks

- Removing a person from Entra does not end their grants: disable the conde user (admin API).
- The consent screen is the only protection against a victim opening an attacker's authorize URL
  (the client must still be an allowed host with that redirect URI in its metadata document).

## Coverage

The review ran the suite without `CONDE_TEST_PG`, so the Postgres variants of
`tests/mcp_remote.rs` did not run there. The fix commit's full `cargo test --workspace` ran with
`CONDE_TEST_PG` on a throwaway PostgreSQL 18 cluster: all nine `mcp_remote` cases ran on both
stores; 498 passed, 0 failed.
