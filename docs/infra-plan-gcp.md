# conde on GCP: one server per workspace, Keycloak realms, conde.cloud (desk, 2026-10-27)

Decisions taken by Krisztián on 2026-10-27, written down so the build has one source. Nothing is created
until the accounts exist (his: Cloudflare, GCP) and the number below is verified. Figures are from the
registrar's price API and the public cloud price lists on the day; re-read at apply time.

## Decisions

- Product address `conde.cloud` (bought at Cloudflare at cost, 20.20 USD a year), nothing else: no
  redirect domains (Krisztián, 2026-10-27). Registrar Cloudflare (at-cost, 2FA, DNSSEC); DNS zone in
  Cloudflare too, so the WAF and DDoS layer sit in front later without a move.
  Not bought: conde.io (Condé Nast defensive registration via CSC, 2013), conde.ai (speculator, name.si),
  conde.app (Afternic, price unknown). Name risk recorded: "Condé" is a guarded media mark; a public launch
  gets a trademark check first.
- One conde server instance per workspace. Workspaces are hostnames under the product domain:
  `dakai.conde.cloud` (replaces conde.nativeai.space; the Dakai board moves here, history and all) and
  `demo.conde.cloud` (friends, invite only). A later company is a third instance, created from the same
  module. No multi-tenant tables, by decision; users of one instance cannot learn that another exists.
- One Keycloak, one realm per workspace (`dakai`, `demo`). Dakai's realm federates the Dakai Entra tenant
  as its identity provider, so Dakai people keep their Microsoft sign-in; the demo realm has local users
  created by invite (MFA off at first, see below). Conde's sign-in becomes a per-instance OIDC issuer (the realm URL) instead of
  the hard-coded Entra tenant check, and an unknown identity is refused unless an admin pre-created the
  user (invite gate). Keycloak's own database is on the same Postgres server.
- One Cloud SQL for PostgreSQL server, private IP only, one database and one owner role per instance
  (`conde_dakai`, `conde_demo`, `keycloak`); the pattern of terraform/meetings in native-infra. No
  in-cluster Postgres: managed backups, PITR and upgrades at this security level outweigh the learning
  value; CloudNativePG stays an option, conde only needs a connection URL.
- GKE Autopilot, private cluster, Cloud NAT for egress (Slack, the identity providers). Gateway API with
  Google-managed certificates as the only public entry; Cloudflare proxies it (orange cloud) once the
  origin is locked to Cloudflare's ranges.
- Secret Manager with External Secrets Operator and Workload Identity; no Vault (unseal, root token and
  HA are exposure, not security, at this size). No Redis: delivery wake-ups are Postgres LISTEN/NOTIFY,
  sessions and receipts are Postgres rows, the login rate limit moves to Cloud Armor at the edge.
- Artifact Registry for the image, a Cloud Storage bucket for the release binaries (replaces stconde/dist),
  Workload Identity Federation for GitHub Actions (no stored key), Cloud DNS not used (zone at Cloudflare).
- Billing: GCP billing account as a Business payments profile in the company's name, EU VAT number
  HU29264276 (issued 2026-10-27 21:45Z; VIES still answered INVALID at 21:57Z, expected valid the next
  day). Tax status is applied when an invoice is issued, so a number verified before the month closes makes
  that month's invoice reverse-charged. Personal card first, company card later (a card swap, both at
  Google and at Cloudflare). Same VAT number on the Cloudflare account (entered 2026-10-27, accepted without a live VIES check).
- The conde repository moves to Krisztián's personal GitHub later (his instruction, 2026-10-27); the
  release workflow is re-pointed then. Not part of the first build.

## Build order

1. Accounts (Krisztián, in person: account creation and card entry are his), both on the company address
   kdobmayer.devops@gmail.com: Cloudflare account with 2FA, GCP account and the Business billing account
   with the VAT ID; the free trial for the learning part.
2. Domain: conde.cloud at Cloudflare, DNSSEC on, zone with nothing but the apex.
3. GCP foundation (Terraform, new root `infra/gcp` in this repo): project, APIs, VPC with a private
   subnet, Cloud NAT, Artifact Registry, Secret Manager, the bucket, the GitHub federation.
4. Cloud SQL server (private IP), the three databases and roles, passwords in Secret Manager.
5. GKE Autopilot, Gateway, External Secrets Operator, Keycloak (its realm export checked into the repo:
   `dakai` with the Entra identity provider, `demo` with local users; admin realm MFA on).
6. conde code: per-instance OIDC issuer and client, the invite gate, the Cloud Armor rate limit replacing
   the in-process one where it matters. Tests first.
7. The two instances from one Kubernetes module: `dakai` and `demo`, each with its own database URL, admin
   key, realm client and hostname.
8. Final: an infrastructure diagram of the finished estate with the `diagram` skill (owner's order, 2026-10-28),
   made only when every piece above is applied, verified and at the reviewed best practice, for his review;
   and an accurate monthly cost of the whole estate (GCP from the billing export and the price list per
   resource as built, Cloudflare: the domain at 20.20 USD a year, 1.68 a month, plus any paid Cloudflare feature
   if one gets used, none today), owner's order the same day.
8b. Admin surfaces behind Identity-Aware Proxy (Keycloak console on its own hostname and backend, Argo CD),
   replacing the IP allowlist; the public boards and realms stay public (owner, 2026-10-28, instead of a VPN).
8c. Sign-in pages (owner, 2026-10-28): a tenant with a company directory never sees a Keycloak form: dakai goes
   straight to Entra (direct issuer), clearscale's realm redirects straight to Google (Identity Provider
   Redirector in the browser flow); only invite-based workspaces (demo) show a password form. All Keycloak
   pages (login, password setup, errors, account) carry the conde branding of web design option B
   ("Editorial", docs/web-ui-v2/options/B), as a custom login theme built into the Keycloak image; done
   after the cutover basics, together with the web front end rebuild.
9. Dakai cutover: export from conde.nativeai.space (the import job pattern of 2026-10-09), import into
   `dakai`, every teammate re-runs `conde setup` with the new board URL, the old Container App stays read
   only for a week, then is removed on Krisztián's go.

## Decisions after the design review (conde-infra-astra, 2026-10-28 00:00Z)

- Organization: Google created a standalone organization at the trial sign-up (kdobmayer-devops-org,
  476201321288); the project conde-prod lives under it. No folders. Billing account 0150DB-1B0576-157A41.
  Terraform runs as terraform@conde-prod.iam.gserviceaccount.com with explicit admin roles (no basic roles),
  impersonated from the admin's ADC locally and from GitHub through Workload Identity Federation later.
  State: gs://conde-tfstate-476201321288 (EU, versioned, public access prevented), one prefix per root.
- Region europe-west3 (Frankfurt) for everything regional; GKE and Cloud SQL co-located.
- GKE Autopilot, private nodes, control plane reached through the DNS-based endpoint with IAM (no public IP
  endpoint, no authorized-network lists), Gateway API (gke-l7-global-external-managed) with a Certificate
  Manager wildcard certificate for *.conde.cloud via DNS authorization, Cloud Armor attached per backend
  with GCPBackendPolicy, backend timeout raised for the SSE streams. One namespace, one Google service
  account and one Kubernetes service account per workspace; deny-by-default NetworkPolicy.
- Cloud SQL for PostgreSQL, Enterprise edition, zonal, PITR and automated backups on, deletion protection
  on, Private Service Connect (no public IP, no private services access), reached by the Cloud SQL Auth
  Proxy sidecar with automatic IAM database authentication: each conde instance connects as its own Google
  service account, no database password exists for conde. Keycloak uses a generated password held in Secret
  Manager. One database and one role per instance; conde's embedded migrations run at server start under
  an advisory lock, so no separate migration role. Recorded as retained runtime DDL authority (the reviewer's
  point): a compromised instance can alter its own schema; a real split needs a separate migration execution
  path, deferred.
- Secrets: the GKE managed Secret Manager CSI add-on mounts secrets as files; no Kubernetes Secret objects
  carry a value, nothing in etcd or git or Terraform state. Two exceptions, both listed in
  infra/gcp/cluster/README.md: Argo CD's own credentials in its namespace (it reads only Kubernetes Secrets), and the
  IAP OAuth client secret (S4), which GCPBackendPolicy reads only from a Kubernetes Secret in the backend's namespace:
  `iap-admin` in `keycloak` and `argocd`, written by Terraform as write-only data from an ephemeral Secret Manager
  read, so the value never enters the plan or the state, and the Secret object is its one copy outside Secret Manager. conde gets `*_FILE` variants of its secret
  variables (CONDE_ADMIN_KEY, OIDC client secret, CONDE_SLACK_BOT_TOKEN); DATABASE_URL carries no password.
- Keycloak: the pinned official image as a Deployment with probes, one installation, realms `dakai` and
  `demo` managed with the keycloak/keycloak Terraform provider (bootstrap import only for the first start).
  The Gateway routes only /realms/<workspace>/*, /resources/* and /js/*; /admin/* and /realms/master/* are
  reachable only from the admin's allowlisted IP through a Cloud Armor rule, so the Terraform provider can
  run from his machine and nobody else sees the admin console. Replaced by S4 (docs/infra-gcp-s4-iap.md): the
  console and the master realm on admin-auth.conde.cloud behind Identity-Aware Proxy, the Terraform provider
  through a port-forward, unconditional denies for the admin paths on auth.conde.cloud. Admin accounts with MFA. Management port
  9000 private.
- Edge: Cloudflare DNS-only (grey cloud) records to the Google load balancer; Cloud Armor is the WAF and
  rate limiter. No second proxy. The application's own rate limits stay (the per-account code-guess limit
  cannot be done at the edge).
- Not changed: the conde SSE stream lifetime (the client reconnects on close); the backend timeout is set
  high instead.

Terraform layout, all under infra/gcp in this repository: `foundation` (APIs, network, NAT, Artifact
Registry, the dist bucket, WIF), `platform` (Cloud SQL, PSC endpoint and DNS, GKE, static IP, certificate,
Cloud Armor policy, Keycloak database), `workspaces` (the Kubernetes objects of each instance from one
module, plus the Keycloak realms). Local run: `GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com`.

## Distribution, CI/CD and the invite flow (owner, 2026-10-28)

- Binaries: linux-x86_64, darwin-arm64, darwin-x86_64; no Windows build. Hosted in the Cloud Storage bucket
  `conde-cloud-dist` (public read, the GCP equivalent of the stconde blob container) under `v<version>/`
  with a `SHA256SUMS`, plus a `latest` pointer. `install.sh` served from https://conde.cloud/install.sh:
  detects OS and arch, downloads the versioned binary, verifies the checksum, installs to ~/.local/bin,
  honours CONDE_VERSION. `conde update` uses the same files. GitHub Releases and a Homebrew tap replace the
  bucket the day the repository is public; the script then points there.
- CI/CD: GitHub Actions in the conde repository (after its move to the owner's personal GitHub; the WIF
  trust is bound to the numeric repository id). Release workflow on a tag: build the server image for
  linux/amd64 to Artifact Registry `conde` and the three CLI binaries to the bucket; deploy workflow sets
  the image on each workspace Deployment through the GKE DNS endpoint with the WIF-impersonated `deploy`
  service account (namespace-scoped RBAC). No stored key anywhere. Terraform stays human-applied from the
  admin's machine (the Keycloak realm provider needs the admin path).
- Invite flow (demo and any later workspace): the admin runs `conde admin invite <name> <email> --board
  demo`, which creates the conde user (the invite gate matches the verified email at first sign-in) and the
  Keycloak user in the realm with a required "set password" action (no OTP on demo at first: owner 2026-10-28,
  MFA would slow friends down; it is one realm setting to switch on later); Keycloak sends the
  account-setup email (SMTP: a Gmail app password on kdobmayer.devops@gmail.com for now, a transactional
  provider when volume needs it). The invite message the friend gets holds three lines: the install
  one-liner, `conde setup --board https://demo.conde.cloud`, and "your agent can run both; the browser
  sign-in is yours". `conde setup` already registers the MCP server and the skill for the agent.

## Revisit after the cutover (owner, 2026-10-28)

- Network layer: GKE Autopilot runs Dataplane V2, which is Cilium (eBPF) with NetworkPolicy enforcement,
  so the tenant isolation today is Cilium-enforced L3/L4 policy. To revisit: FQDN-based egress policies
  (Dataplane V2 supports them), Cilium cluster-wide policies, and whether a service mesh earns its place:
  Cloud Service Mesh (managed Istio, ambient mode) for mTLS and L7 authorization between namespaces, with
  the Gateway feeding the mesh ingress. Not now: no two workloads talk to each other inside the cluster
  except conde to Keycloak over the public hostname, and a mesh is a component with no requirement yet.
- Identity-aware access (IAP) for the admin surfaces, replacing the IP allowlist (8b above).
- Keycloak file vault for the Google and SMTP secrets so they leave Terraform state (review finding).

## Open

- The Cloudflare proxy in front of the Gateway needs origin locking (Cloudflare IP ranges or an
  Authenticated Origin Pull certificate) or the WAF is bypassable; decided at step 5.
- The conde SSE streams through the Cloudflare proxy: the free plan's 100 s idle timeout versus conde's
  stream keepalive; measured at step 7, DNS-only mode is the fallback for the instance hostnames.

## As built (2026-10-28)

Where the code under infra/gcp, charts and deploy differs from the plan above:

- Six Terraform roots instead of three: `foundation`, `platform`, `cluster` (the Gateway, Keycloak, Argo CD and
  Identity-Aware Proxy for the admin backends), `workspaces` (only the GCP side of each workspace: the database and
  its IAM user, the service accounts, the secrets), `realms` (the Keycloak configuration, run through a port-forward) and `dns` (the
  Cloudflare records and the Pages project), with the shared module in `modules/workspace`.
- No `dakai` realm. Dakai signs in directly against its Entra tenant through its existing app registration; the
  realms are `demo` (passwords), `clearscale` (Google sign-in), `infra` (platform tools, MFA) and `master`.
- No deploy workflow and no `deploy` access to the cluster. The release workflow commits the new image digest to
  `deploy/image.yaml` on main, and Argo CD rolls out every workspace from `charts/conde-workspace` and
  `deploy/tenants/<name>.yaml`.
- Secrets reach the pods through the GKE Secret Manager CSI add-on as files; External Secrets Operator is not
  installed (the chart keeps it only as an option for clusters outside GCP).
- Mail goes through Resend, not a Gmail app password: Keycloak's SMTP for every realm and the conde server's invite
  email share the `resend-api-key` secret, and the `dns` root holds the Resend DKIM, SPF and bounce records.
- Outbound traffic from the workspaces, Keycloak and Argo CD goes only to named hosts (GKE FQDN network policies,
  docs/infra-network-s7.md), not to any public address on 443 and 465 or to hand-copied GitHub ranges.
- The apex `conde.cloud` and `www.conde.cloud` are proxied CNAMEs to Cloudflare Pages, which serves the landing
  page, the documentation and install.sh; only the wildcard for the workspace and Keycloak hostnames points at the
  Google load balancer, DNS only.
