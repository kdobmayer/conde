# Components of the estate drawing

Every element drawn, grouped as drawn, with the source it was read from at master `45a8f57`.
Paths are relative to the repository root.

## Owner, teammate, Cloudflare (top band)

- Owner, kdobmayer.devops@gmail.com: `infra/gcp/cluster/iap.tf` (`iap_member`), `infra/gcp/realms/main.tf` (`owner_email`).
- Terraform as terraform@conde-prod, state in conde-tfstate: `infra/gcp/*/versions.tf` (gcs backend
  `conde-tfstate-476201321288`, `impersonate_service_account`).
- Owner's browser to the Keycloak console and Argo CD, signed in to IAP by Google: `infra/gcp/cluster/README.md` (Argo CD),
  `infra/gcp/realms/README.md`, `docs/infra-gcp-s4-iap.md`.
- Teammate: conde CLI and agent (board, MCP server, skill), browser (web app, sign-in): `docs/infra-plan-gcp.md`
  (invite flow, `conde setup`).
- Installer: install.sh from conde.cloud, binary from conde-cloud-dist: `.github/workflows/site.yml` (install.sh
  served by the site), `.github/workflows/release.yml` (publish job), `docs/infra-plan-gcp.md` (Distribution).
- Cloudflare zone conde.cloud: Pages project `conde-cloud` on conde.cloud and www, proxied; `*.conde.cloud` A record
  to the Gateway address, DNS only; certificate authorization record; Resend DKIM, SPF and MX on `send`, DMARC:
  `infra/gcp/dns/main.tf`.

## GCP project conde-prod, edge stripe

- Project conde-prod, europe-west3: `infra/gcp/foundation/main.tf` (`locals`).
- GKE DNS endpoint, IAM only, no IP endpoint: `infra/gcp/platform/gke.tf` (`control_plane_endpoints_config`);
  Terraform kubernetes and helm providers on it: `infra/gcp/cluster/main.tf`; port-forward to Keycloak:
  `infra/gcp/realms/run.sh`.
- Gateway conde, global external Application Load Balancer, HTTPS and HTTP redirect: `infra/gcp/cluster/gateway.tf`
  (`gke-l7-global-external-managed`, listeners, `https-redirect`).
- Certificate Manager, conde.cloud and *.conde.cloud: `infra/gcp/platform/edge.tf`.
- Static IP conde-gateway: `infra/gcp/platform/edge.tf` (`google_compute_global_address.gateway`).
- Public hosts, Cloud Armor conde-web (admin and master paths denied, sign-in rate ban, L7 DDoS defense):
  `infra/gcp/platform/edge.tf` (`google_compute_security_policy.web`); attached to the workspaces:
  `charts/conde-workspace/templates/gcp-policies.yaml`, to Keycloak: `infra/gcp/cluster/keycloak.tf` (`keycloak_backend`).
- Admin hosts, Cloud Armor conde-keycloak-admin and conde-argocd (per-IP throttle): `infra/gcp/platform/edge.tf`
  (`admin_policies`).
- Identity-Aware Proxy, only the owner's Google account, Google signs the browser in: `infra/gcp/cluster/iap.tf`; on keycloak-admin:
  `infra/gcp/cluster/keycloak.tf` (`keycloak_admin_backend`); on argocd-server: `infra/gcp/cluster/argocd.tf`
  (GCPBackendPolicy in `extraObjects`).

## VPC conde and the GKE cluster

- VPC conde, subnet gke-europe-west3 10.10.0.0/20: `infra/gcp/foundation/main.tf`.
- GKE Autopilot conde, private nodes, Secret Manager CSI add-on: `infra/gcp/platform/gke.tf`. Dataplane V2:
  `docs/infra-plan-gcp.md` (Revisit after the cutover). Deny-by-default NetworkPolicy: `infra/gcp/cluster/keycloak.tf`,
  `infra/gcp/cluster/argocd.tf`, `charts/conde-workspace/templates/networkpolicy.yaml`.
- Workspaces ws-dakai, ws-demo, ws-clearscale from charts/conde-workspace: `deploy/tenants/*.yaml`,
  `deploy/argocd/workspaces.yaml.tftpl`. Hosts: same files. conde replicas: dakai 2 (`charts/conde-workspace/values.yaml`
  default, no override), demo and clearscale 1 (`replicas: 1` in `deploy/tenants/demo.yaml`, `clearscale.yaml`). Auth Proxy sidecar with IAM database auth:
  `charts/conde-workspace/templates/_helpers.tpl` (`--auto-iam-authn`). Secrets as CSI files (admin key, OIDC client
  secret, Slack token, Resend key, and on demo the Keycloak admin client secret):
  `charts/conde-workspace/templates/secretproviderclass.yaml`, `deploy/argocd/workspaces.yaml.tftpl`. Sign-in: Dakai Entra
  tenant, realm demo, realm clearscale: `deploy/tenants/*.yaml`. Databases conde_<name>:
  `infra/gcp/modules/workspace/main.tf`.
- Keycloak, Service keycloak on auth.conde.cloud (workspace realms, infra), Service keycloak-admin on
  admin-auth.conde.cloud (console, realm master), Keycloak 26.8.1, 1 replica, conde theme, secrets and file vault as
  CSI files, Cloud SQL Auth Proxy sidecar with password auth: `infra/gcp/cluster/keycloak.tf`.
- Realms demo (passwords, invites), clearscale (Google, clearscale.com), infra (Argo CD sign-in, MFA), master
  (console, Terraform client, MFA), mail through Resend SMTPS: `infra/gcp/realms/main.tf`, `realms.tf`, `infra.tf`,
  `master.tf`, `README.md`.
- Argo CD, chart 10.10.3, argocd-server on argocd.conde.cloud, server, repo-server, controller, applicationset,
  redis, sign-in through realm infra via the Gateway: `infra/gcp/cluster/argocd.tf` (`argocd_oidc_issuer`, egress to
  the Gateway address). ApplicationSet conde-workspaces, one ws-<name>
  per deploy/tenants file: `deploy/argocd/workspaces.yaml.tftpl`.
- Cloud NAT conde, public internet egress: `infra/gcp/foundation/main.tf` (`google_compute_router_nat.conde`); Google
  APIs go through Private Google Access (`private_ip_google_access` on the subnet), the database through PSC.
- Kubernetes Secrets with values: `iap-admin` in keycloak and argocd, written by Terraform as write-only data from
  Secret Manager (`infra/gcp/cluster/iap.tf`); Argo CD's `argocd-secret`, `argocd-redis` and `repo-conde`:
  `infra/gcp/cluster/README.md` (Kubernetes Secrets that hold values).
- Private Service Connect psc-conde-pg, private DNS zone: `infra/gcp/platform/sql.tf`.

## Managed services

- Cloud SQL conde-pg, PostgreSQL 18, zonal, PITR, no public IP, IAM auth on, databases keycloak and
  conde_<workspace>: `infra/gcp/platform/sql.tf`, `infra/gcp/modules/workspace/main.tf`.
- Secret Manager, generated values written write-only: `infra/gcp/platform/secrets.tf`,
  `infra/gcp/modules/workspace/main.tf`, `infra/gcp/realms/main.tf`.
- conde-tfstate: `infra/gcp/*/versions.tf`, `docs/infra-plan-gcp.md`.
- Artifact Registry conde, images conde and keycloak, 20 newest kept: `infra/gcp/foundation/main.tf`,
  `infra/gcp/cluster/main.tf` (`keycloak_image`), `deploy/image.yaml`.
- conde-cloud-dist, public read, binaries, SHA256SUMS, latest: `infra/gcp/foundation/main.tf`,
  `.github/workflows/release.yml` (publish).
- Workload Identity Federation, pool github, service account deploy: `infra/gcp/foundation/main.tf`.

## Internet services and GitHub

- Dakai Entra tenant, OIDC sign-in of dakai: `deploy/tenants/dakai.yaml`, `infra/gcp/workspaces/main.tf`.
- Google: Keycloak's clearscale sign-in, the broker back-channel: `infra/gcp/realms/realms.tf`
  (`keycloak_oidc_google_identity_provider`).
- Resend: Keycloak SMTPS 465: `infra/gcp/realms/main.tf` (`smtp`), `infra/gcp/cluster/keycloak.tf` (egress 465);
  conde invite email API: `charts/conde-workspace/values.yaml` (`invite`), `crates/conde-remote/src/web/invite.rs`.
- Slack: conde bot messages: `charts/conde-workspace/templates/deployment.yaml` (`CONDE_SLACK_BOT_TOKEN_FILE`),
  `crates/conde/src/slack.rs`.
- GitHub repository kdobmayer/conde, pulled by Argo CD over SSH, branch main: `infra/gcp/cluster/argocd.tf`,
  `infra/gcp/cluster/variables.tf` (`gitops_revision`).
- GitHub Actions, release on a v* tag, promote to deploy/image.yaml, site to Cloudflare Pages:
  `.github/workflows/release.yml`, `.github/workflows/site.yml`.

## Edges

- Owner's browser, teammate's CLI and browser to the Gateway; Gateway to the public and admin hosts; public hosts to
  the workspaces and Service keycloak; admin hosts to Service keycloak-admin and argocd-server: the HTTPRoutes in
  `infra/gcp/cluster/keycloak.tf`, `argocd.tf`, `charts/conde-workspace/templates/httproute.yaml`.
- Installer to Pages (install.sh): `.github/workflows/site.yml`.
- Terraform to the GKE DNS endpoint to the cluster: `infra/gcp/cluster/main.tf`, `infra/gcp/realms/run.sh`.
- Workspaces to Keycloak, OIDC of demo and clearscale out to auth.conde.cloud and back through the Gateway and
  conde-web: issuers in `deploy/tenants/demo.yaml`, `clearscale.yaml`, discovery from the issuer in
  `crates/conde-remote/src/web/auth.rs`. Demo's admin API, in-cluster HTTP to
  `http://keycloak.keycloak.svc.cluster.local:8080`: `charts/conde-workspace/values.yaml` (`invite.keycloakAdmin`),
  `charts/conde-workspace/templates/networkpolicy.yaml`, `infra/gcp/cluster/keycloak.tf` (ingress from ws-demo).
- Cluster to Cloud NAT to the internet services and GitHub: the egress lists in `infra/gcp/cluster/main.tf`
  (`network_policy_egress`), `keycloak.tf` (465), `argocd.tf` (GitHub ranges on 22 and 443).
- Auth Proxy sidecars to Private Service Connect to Cloud SQL: `infra/gcp/platform/sql.tf`, the `--psc` proxy flag.
- CSI files from Secret Manager: the SecretProviderClasses in `infra/gcp/cluster/keycloak.tf`,
  `charts/conde-workspace/templates/secretproviderclass.yaml`.
- Image pull from Artifact Registry: `infra/gcp/cluster/main.tf` (`nodes_pull`).
- GitHub Actions through Workload Identity Federation to the image and the binaries, promote commit:
  `.github/workflows/release.yml`, `infra/gcp/foundation/main.tf`.

## In the source, not drawn

The dbinit Jobs, the gateway namespace (its Gateway and redirect route are the Gateway node), the private DNS zone as
its own node (named in the PSC note), the Argo CD repository import hook, the node service account, Cloud Logging and
Monitoring, the Argo CD sync of the workspaces as a line (named in the controller and ApplicationSet notes), the
Terraform state access as a line (named in the Terraform note), the site deploy to Pages as a line (named in the
Actions note).
