# conde backlog

The standing queue for contributors. Pull from the top; claim an item with one
line on your thread with desk ("taking B3") before starting, so no two lanes
take the same item. Each item names its area (files that change) so two lanes
can run in parallel when their areas do not overlap. Details and history live
in docs/NEXT.md (section letters in brackets). Rules for every item: branch
from the fresh main, one PR, one commit when the parts share files, one-line
commit messages, no attribution, `cargo test --workspace` green on both stores
(CONDE_TEST_PG; on the release machine also CONDE_STRICT_TIMING=1 for the tight
timing bounds), no new clippy warning, docs updated where behavior changes.
Review and integration by desk, one batch at a time; one release per batch.

## conde.cloud on GCP: the remaining sequence (owner, 2026-10-28)

Owner's ruling: nothing below is deferred except D1 (needs his hand) and the network-layer revisit, which is
the last step. Desk runs the rest in this order after the Dakai cutover basics (release, rehearsal, cutover).

- S1 (desk): the invite email flow: `conde admin invite <name> <email> --board <ws>`, the join page
  `/join/<token>` (set password, then the one line for the agent), Keycloak account creation through its
  admin API, mail through Resend; `conde update`; the site documents both.
- S2 (desk): the web front end v2 in design B, Editorial (docs/web-ui-v2/plan.md items U1 to U16, with the
  owner's 2026-10-28 choice of B: skin A is not built further and U16 removes it; the plan's "dark only"
  stays unless the owner says otherwise). Landed on the `web-v2` integration branch, one merge to main, as
  the plan prescribes. The Keycloak login theme (S2b) is built from the same tokens right after.
- S2b (desk): Keycloak login theme in the conde branding, design B (docs/infra-plan-gcp.md 8c).
- S3 (desk): the landing page second pass with the owner's illustration (text-free variant by Astra,
  lowercase "conde", under the headline at about 60% width; a square crop for the social preview); two
  rendered options for the owner: a three-step strip, a short real agent-human exchange in the serif, the
  three sign-in modes as a footer fact. Source: site/src/assets/source/landing-illustration-v1.png.
- S4 (desk): Identity-Aware Proxy in front of the admin surfaces (Keycloak console on its own hostname and
  backend, Argo CD), replacing the IP allowlist (docs/infra-plan-gcp.md 8b); a Cloud Armor policy of its own
  for the Argo CD backend.
- S5 (desk): Keycloak file vault for the Google and SMTP secrets so they leave Terraform state.
- S6 (desk): delete the Azure conde resources once the Dakai cutover is a week old (2026-11-04 at the earliest;
  the resource list shown to the owner before the destroy, per his rule); the DakaiGroup/conde repository was
  deleted by the owner on 2026-10-28.
- S1b (desk, after S1): follow-ups from the S1 review: the Keycloak `conde` email theme hard-codes the demo
  workspace name and URL (a second password realm needs a realm-aware template); every realms apply that changes
  the demo realm drops and re-adds the `conde-admin` manage-users role for a few seconds (invites fail in that
  window; a dependency fix in infra/gcp/realms). Done: inviting a pre-existing realm account (the realm-clear
  admin call, bc6bfd1).
- S2c: done 2026-10-28 (account console theme, master display name "conde admin", image 26.8.1-conde7). Open:
  the unused Keycloak images 26.8.1-conde1 to conde6 in Artifact Registry go with the next registry cleanup
  (owner's deletion list).
- S8: done 2026-10-28 (master 21c25b5). Open: the Secret Manager secret `keycloak-bootstrap-admin` is out of
  Terraform state and still in GCP (owner's deletion list); the `deploy` service account's display name still
  says "GKE rollout" (rides the next foundation change).
- S9 (desk, after v0.5.0): what remains of the old board's names: the launchd label `space.nativeai.conde`
  (a rename needs a migration of existing installs), and the README's install, dist and readme URLs at
  `conde.nativeai.space` (the served README's test pins them; the invite page already says
  `conde.cloud/install.sh`); test fixtures keep `dakai` as a value. An older daemon drops the welcome from
  board.info during an upgrade (harmless, prints the default guide; the fix is a direct board.info read in setup
  when the daemon's answer has none, more than a one-line read, so left). Done: the shipped skills name the
  host's default shared board, else its first enrolled one, rendered by `conde setup` (`<alias>` with none),
  and the README's board example is `acme` at `acme.conde.cloud` (s9b branch); CI's Postgres service and
  CONDE_TEST_PG, the Postgres-only skips, admins on the web view and the SIGTERM ordering (s9 branch).
- S7 (desk, last): the network layer revisit: FQDN egress policies, Cilium cluster-wide policies, Cloud
  Service Mesh if a pod-to-pod requirement appears, Cloudflare Tunnel plus Access as an alternative edge.

Owner's own steps, at his pace, none blocking the above:
- D1: replace the GitHub Actions secret CLOUDFLARE_API_TOKEN (today the conde-terraform token: DNS edit +
  Zone settings + Pages edit) with a token holding only "Cloudflare Pages: Edit": create it at
  dash.cloudflare.com/profile/api-tokens, `pass insert -f conde/cloudflare-pages-token`, then desk runs
  `pass show conde/cloudflare-pages-token | gh secret set CLOUDFLARE_API_TOKEN -R kdobmayer/conde`.
- D2: sign in to Keycloak's admin console as the permanent `admin` (temporary password in Secret Manager
  keycloak-admin-temp-password), set the password and the authenticator; then desk deletes bootstrap-admin
  and adds master to `no_password_grant` (infra/gcp/realms/README.md).
- D3: sign in to Argo CD at argocd.conde.cloud through the infra realm as `krisztian` (temporary password
  in Secret Manager infra-krisztian-temp-password); then desk flips `argocd_local_admin` to false and
  deletes argocd-initial-admin-secret.
- D4: Cloudflare account 2FA (profile, Authentication); it holds the domain.
- D6 (after S5): the realms Terraform state no longer holds the Google client secret and the Resend key, but the
  state bucket keeps old object versions that do. Cheapest fix: rotate both (Resend dashboard: new API key, then
  `pass insert -f conde/resend-api-key` and a new Secret Manager version of `resend-api-key`; Google Auth Platform:
  add a new secret on the client keycloak-clearscale, same for `clearscale-google-oauth-client-secret`); Keycloak
  picks up new versions within two minutes, no restart. The alternative is deleting the old state versions in
  gs://conde-tfstate-476201321288 (destructive; desk shows the list first).
- D5 (for S4, IAP): create the IAP OAuth client in the Google Auth Platform of conde-prod, because its secret is
  shown once: https://console.cloud.google.com/auth/clients?project=conde-prod, Create client, Web application,
  name `iap-admin`, Create; copy the client ID and the secret; edit the client and add the redirect URI
  `https://iap.googleapis.com/v1/oauth/clientIds/<client ID>:handleRedirect`, Save. Then in a terminal:
  `read -rs S && printf '%s' "$S" | pass insert -m conde/iap-admin-client-secret && unset S` and
  `pass show conde/iap-admin-client-secret | gcloud secrets create iap-admin-client-secret --replication-policy=user-managed --locations=europe-west3 --data-file=- --project conde-prod`.
  Send desk the client ID (not the secret). The audience of the "conde" app must be External (it is).

## Standing items (no claim needed)

- S1. Release smoke on the NUC after every release, unattended with the
  existing enrollment: upgrade-install from
  https://conde.nativeai.space/install.sh (checksum), `conde --version` shows
  the release build; `conde setup` (rewrites skills and MCP entries that the
  release changed, reports unchanged otherwise); restart the bridge, `conde
  doctor` all clear (the bridge runs the new build), `conde setup --check` all
  clear; the bridge reconnects
  to dakai with the existing host credential (no re-login); one DM each way
  with desk. The interactive login (`conde login --server ... --board <alias>
  --code`) is exercised only when a release changes the login path and a human
  is around; otherwise the report says "login not exercised". One line when
  green; full detail when not.

## Queue, top first

- B11. [J] `conde admin key create` adds a key without revoking the host's
  live ones (the only overlap path; every other mint revokes and inserts in
  one transaction). Decided: revoke by default like the other mints, with
  `--keep-live` for a hand-over rotation; the command prints which it did.
  Area: crates/conde admin CLI + ops::hosts, tests.
- B20. install.sh staging dir: the new binary is staged under the default temp
  directory and moved over the target, so the final mv is a copy when temp and
  the target are on different filesystems (not atomic); stage next to the target
  instead. Area: dist/install.sh, dist_files test.

- B21. Admin users act in the web view (invite, disable) without the admin key: later.

## Waiting on a decision or an observation (do not take)

- W1. [I] Refresh reuse grace (a token consumed less than 10 s ago answers
  invalid_grant without revoking) and client-metadata pinning per host: both
  wait for the owner's first remote MCP connect and its log.
- W3. [D] Web UI images: the brief is in NEXT.md section D; generation goes
  through the owner's design partner, not a contributor lane.
- W4. Web UI overhaul (owner, 2026-10-14: the current layout and visuals are
  not good enough; the new UI must be well thought out, visually striking,
  well organized) with Konstantin's live Feed folded in (every message across
  the owner's agents, thread view, filters, live push, owner-scoped,
  read-only). Design phase at desk first (audit, information architecture,
  three rendered directions, plan); implementation items appear here after
  the owner picks a direction.

## Web UI v2 (owner 2026-10-14: skins A Console and B Editorial over one skeleton, per-browser switch, dark only; C not built; pick or mix after real traffic)

Design: docs/web-ui-v2/ (audit.md, ia.md, options/, plan.md). Each item below
is specified in plan.md with its gate; the lines here are the index. Server
items (S) run one at a time; client groups C1, C2, C3 are disjoint files and
can run in parallel with S; U6-U15 go to the branch web-v2, U16 merges it.

- U6. App shell, routing and the skin switch (default A). Area: web/src/routes/+layout.svelte, routes/+page.svelte, routes/{t,agents,a,channels,c,fleet}/**, lib/skins/**, lib/nav.svelte.ts, lib/UserMenu.svelte, app.css, app.html, test/skin.test.mjs. C1, after U1.
- U7. Data layer with the window rule (floor and exhausted, empty-after-pass restart, 10-page bound, 2 s cadence). Area: web/src/lib/{feed-window.ts,feed.svelte.ts,thread-view.svelte.ts,thread-tree.ts,types.ts}, scripts/design/mock-server.mjs, test/{feed,thread}.test.mjs. C1.
- U8. Filter menu, sheet, disclosure with interaction checks; state components. Area: web/src/lib/ui/**, routes/_design/primitives/+page.svelte, scripts/design/interact.mjs, test/ui.test.mjs. C1.
- U9. Feed and thread pages, plus the skin-switch preservation check. Area: web/src/routes/+page.svelte, routes/t/[id]/+page.svelte, lib/Thread.svelte, lib/FeedList.svelte, scripts/design/{shoot,interact}.mjs. C2, after C1.
- U10. Conversation, Agents and Channels pages. Area: web/src/lib/{Conversation,Delivery}.svelte, routes/{a,agents,c,channels}/**, test/conversation.test.mjs. C2.
- U11. Fleet page. Area: web/src/routes/fleet/**. C2.
- U12. Skin A, Console. Area: web/src/lib/skins/a/**. C3, after C2.
- U13. Skin B, Editorial. Area: web/src/lib/skins/b/**. C3.
- U14. Keys page restyle (outside the switch). Area: crates/conde-remote/src/web/pages.rs, web/scripts/design/mock-server.mjs. C3.
- U15. Fonts for skin B (latin + latin-ext woff2, SIL OFL). Area: web/static/app/fonts/**, web/src/lib/skins/b/fonts.css, web/src/lib/design/fixture.ts, web/README.md. After U13.
- U16. Remove the design routes; gate web-v2 on the real server (smoke on both stores); merge web-v2 to main. Area: web/src/routes/_design/**, routes/+layout.svelte, lib/design/model.ts, scripts/design/interact.mjs, crates/conde/tests/web_ui.rs, docs/web-ui-v2/options/README.md. Last, after U1-U5 on main and U6-U15 on web-v2.
- W-UI. The owner's A/B pick (or mix) after real traffic, then delete the other skin and the switch. Do not take.
