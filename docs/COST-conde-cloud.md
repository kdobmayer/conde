# Monthly cost of the conde.cloud estate

Read on 2026-10-28 from the Terraform roots under `infra/gcp` and `infra/terraform`, the live project `conde-prod`
(read-only `gcloud`, `kubectl` and the Cloud Monitoring API as `terraform@conde-prod`), the live Azure subscription
(read-only `az` and the Cost Management API), and the public price pages named per row. Prices are the list prices
for `europe-west3` (Frankfurt) in USD; a month is 730 hours. Refreshed the same day after the cost cuts (one replica
for demo and clearscale, smaller sidecars and Keycloak, the GKE metric packages off) and S4 (Identity-Aware Proxy in
front of the admin surfaces).

The estate costs **243.1 USD a month** (218.0 EUR at the ECB reference rate of 1.1150 USD per EUR, 2026-10-27),
of which GCP is 241.4 USD, Cloudflare 1.68 USD, Resend and GitHub Actions nothing. The biggest item is GKE Autopilot
pod compute at 118.7 USD (49 percent): the four workspace pods and their Cloud SQL proxy sidecars are 61.2 USD. Demo
and clearscale run one replica each and accept the single-replica outage the two replicas were introduced against
(nine minutes on 2026-10-09, `infra/terraform/conde.auto.tfvars`); dakai keeps two. Until the Azure removal (S6) the
old estate runs in parallel at 89.8 USD a month, measured, so the outlay today is 333 USD a month.

## GCP, per resource

| Resource | How it is billed | Quantity as built | Unit price (source, read 2026-10-28) | USD per month |
|---|---|---|---|---|
| GKE Autopilot pods, workspaces (`charts/conde-workspace`, `deploy/tenants`) | per second of requested vCPU, GiB memory, GiB ephemeral storage of running pods; Autopilot fills in a 1 GiB ephemeral default where none is requested | 4 pods: dakai 2 replicas, demo and clearscale 1 each (`deploy/tenants`); each pod `conde` 250 m / 512 Mi / 1 Gi plus the `cloud-sql-proxy` sidecar 50 m / 64 Mi / 100 Mi; live sum 1.2 vCPU, 2.25 GiB, 4.39 GiB (read from the pods, `kubectl get pods -A -o json`) | 0.0573 USD per vCPU-hour, 0.0063421 per GiB-hour, 0.0001789 per GiB-hour ephemeral, Frankfurt table on https://cloud.google.com/kubernetes-engine/pricing | 61.19 |
| GKE Autopilot pods, Keycloak (`cluster/keycloak.tf`) | same | 1 pod: `keycloak` 250 m / 768 Mi / 1 Gi plus proxy sidecar 50 m / 64 Mi / 100 Mi = 0.3 vCPU, 0.81 GiB, 1.1 GiB | same | 16.45 |
| GKE Autopilot pods, Argo CD (`cluster/argocd.tf`, chart argo-cd 10.10.3) | same | controller 250 m / 512 Mi, repo-server 250 m / 512 Mi, server 100 m / 256 Mi, applicationset 100 m / 128 Mi, redis 100 m / 128 Mi; 1 GiB ephemeral each by Autopilot default = 0.8 vCPU, 1.5 GiB, 5 GiB | same | 41.06 |
| GKE cluster fee | 0.10 USD per cluster-hour; the free tier gives 74.40 USD a month per billing account for one Autopilot or zonal cluster | 1 cluster, the only one on the billing account | 0.10 USD per hour and the 74.40 USD credit, https://cloud.google.com/kubernetes-engine/pricing | 73.00 fee, 74.40 credit: 0.00 |
| Cloud SQL `conde-pg` compute (`platform/sql.tf`) | per hour of vCPU and memory, Enterprise edition, zonal (no HA uplift) | `db-custom-1-3840`: 1 vCPU, 3.75 GiB; POSTGRES_18, zone europe-west3-b | 0.0496 USD per vCPU-hour (36.208 per month), 0.0084 per GiB-hour (6.132 per month), Enterprise General Purpose, Frankfurt, https://cloud.google.com/sql/pricing | 59.20 |
| Cloud SQL storage | per GiB-month, SSD, zonal | 20 GiB PD_SSD (autoresize on) | 0.204 USD per GiB-month, Frankfurt, same page | 4.08 |
| Cloud SQL backups and PITR logs | per GiB-month of backup storage used | 7 retained automated backups, 7 days of transaction logs (in Cloud Storage); one backup exists so far, size not reported by the API | 0.096 USD per GiB-month, Frankfurt, same page | 0.10 (estimate, 1 GiB) |
| Cloud Monitoring, Managed Service for Prometheus | per million samples ingested, no free allotment | the metric packages are off (`monitoring_config` in `platform/gke.tf` enables `SYSTEM_COMPONENTS` only; system metrics are free); measured: no samples ingested after 06:27 UTC on 2026-10-28, against 533 thousand an hour before | 0.06 USD per million samples, https://cloud.google.com/stackdriver/pricing | 0.00 |
| Global external Application Load Balancer (Gateway `conde`, `cluster/gateway.tf`) | per forwarding-rule hour (first five rules share one charge) plus data processed | 2 global forwarding rules (HTTP redirect and HTTPS) on one static address; measured traffic 19 MB a day through the balancer | 0.025 USD per hour for the first five rules; 0.008 USD per GiB processed (the monthly view of the same Frankfurt table prints 0.01), https://cloud.google.com/vpc/network-pricing | 18.26 |
| Cloud Armor policies `conde-web`, `conde-keycloak-admin`, `conde-argocd` (`platform/edge.tf`), Standard tier | per policy-hour, per rule-hour, per million requests evaluated | 3 policies, 9 rules: `conde-web` 5 (4 custom plus the default allow), each admin policy 2 (a per-IP throttle plus the default allow); every request meets one policy, measured 453 requests an hour, 330 thousand a month; Adaptive Protection has no Standard-tier price on the page | 0.006849315 USD per policy-hour (5.00 a month), 0.001369863 per rule-hour (1.00 a month), 0.75 per million requests, https://cloud.google.com/armor/pricing | 24.25 |
| Private Service Connect endpoint `psc-conde-pg` (`platform/sql.tf`) | per endpoint-hour plus consumer data processed | 1 forwarding rule; database traffic a few GiB a month | 0.01 USD per hour, 0.01 per GiB, https://cloud.google.com/vpc/network-pricing | 7.30 |
| Cloud NAT `conde` (`foundation/main.tf`) | per assigned VM-hour, per external-address-hour, per GiB processed | 2 Autopilot nodes, 1 auto-allocated address; measured 618 MB processed on the first day (image pulls), taken as 3 GiB a month | 0.0014 USD per VM-hour, 0.005 per address-hour, 0.045 per GiB, https://cloud.google.com/vpc/network-pricing | 5.83 |
| Secret Manager, active versions | per active secret version-hour per replica location, first 6 free | 20 secrets, 1 active version each, 1 location | 0.000082192 USD per version-hour (0.06 a month), https://cloud.google.com/secret-manager/pricing | 0.84 |
| Secret Manager, access operations | per 10,000 AccessSecretVersion calls, first 10,000 a month free | measured 1,174 calls in the hour 07:00 to 08:00 UTC on 2026-10-28, 857 thousand a month; the callers are the workspace and Keycloak service accounts, consistent with the Secret Manager CSI driver re-reading every mounted secret on its rotation poll (`secret_manager_config.rotation_config` in `platform/gke.tf`) | 0.03 USD per 10,000, same page | 2.54 |
| Cloud DNS private zone `conde-pg-psc` | per zone-hour, first 25 zones; queries per million | 1 private zone, queries from the cluster | 0.000273973 USD per zone-hour (0.20 a month), 0.40 per million queries, https://cloud.google.com/dns/pricing | 0.20 |
| Cloud Storage `conde-cloud-dist`, `conde-tfstate-476201321288` | per GiB-month Standard, operations, internet egress | 195 MB and 2 MB stored; release downloads leave at 0.12 USD per GiB | 0.023 USD per GiB-month Standard Frankfurt, 0.005 per 1,000 class A operations, https://cloud.google.com/storage/pricing | 0.01 |
| Internet egress through the load balancer | per GiB, Premium tier, from Frankfurt | measured 0.6 GiB a month | 0.12 USD per GiB to Europe and North America, Frankfurt table, https://cloud.google.com/vpc/network-pricing | 0.07 |
| Artifact Registry `conde` | per GiB-month, first 0.5 GiB free | 348 MB stored; the cleanup policy keeps the 20 newest images | 0.10 USD per GiB-month above 0.5 GiB, https://cloud.google.com/artifact-registry/pricing | 0.00 |
| Certificate Manager (`platform/edge.tf`) | per certificate-month, first 100 free | 1 managed certificate (apex and wildcard), 1 map, 1 DNS authorization | 0.00 for the first 100, https://cloud.google.com/certificate-manager/pricing | 0.00 |
| Cloud Logging | per GiB ingested into the `_Default` bucket above 50 GiB a month, 30 days retention included | measured 31 MB an hour (container 16.5, node 11, the rest 3.5), 21 GiB a month | 0.50 USD per GiB above 50 GiB, https://cloud.google.com/stackdriver/pricing | 0.00 |
| Static external address `conde-gateway` | free while attached to a forwarding rule | 1 global address, in use | https://cloud.google.com/vpc/network-pricing | 0.00 |
| **GCP total** | | | | **241.38** |

## Outside GCP

| Item | How it is billed | Quantity | Unit price (source) | USD per month |
|---|---|---|---|---|
| Cloudflare: domain `conde.cloud` | yearly registration at cost | 1 domain; DNS zone and the Pages project `site` are on the free plan, no paid feature in use | 20.20 USD a year (owner, `docs/infra-plan-gcp.md`) | 1.68 |
| Resend | free plan: 3,000 emails a month, 100 a day, 3 domains, 30 days retention; the next plan is Pro at 20 USD a month for 50,000 emails | invite emails only | https://resend.com/pricing | 0.00 |
| GitHub Actions, repository `kdobmayer/conde` (private, personal account) | included minutes per month on the personal plan: 2,000 on Free, 3,000 on Pro (the plan is on https://github.com/settings/billing; the API scope of this machine cannot read it); macOS draws 10 included minutes per minute, Windows 2, Linux 1; beyond the allowance Linux 2-core 0.006 USD a minute, macOS 0.062 | a release run is two macOS jobs of 1 m 40 s each and about 17 Linux job-minutes (57 included minutes); a CI run is about 15 Linux minutes; on 2026-10-28 there were 3 releases and 7 CI runs (about 280 included minutes in one day) | https://docs.github.com/en/billing/concepts/product-billing/github-actions | 0.00 within the allowance |
| **Total** | | | | **243.06** |

GitHub Actions per release, beyond the allowance: 4 macOS minutes (each job rounded up) at 0.062 plus 17 Linux minutes at 0.006 is 0.35 USD. In included minutes a release consumes 57 and a CI run about 15. The build-up cadence of 2026-10-28 (3 releases, 7 CI runs) would exhaust a Free plan's 2,000 minutes in about a week; the steady cadence of one release and a handful of CI runs a week (about 300 minutes a month) stays inside it. The macOS multiplier is the one GitHub documents for included minutes; the English page no longer prints the table on 2026-10-28, the Japanese page still lists Linux 1, Windows 2, macOS 10.

## Trial credit

The 300 USD trial credit covers 37 days of the GCP estate at 241.4 USD a month (7.94 USD a day). The project was
created on 2026-10-27 at 23:16 UTC and ran its first seven hours at the earlier 358.9 USD a month (about 3.40 USD);
if the credit was whole at creation it runs out around 2026-12-04. The credit's
real balance and the first invoice are console-only (Billing, Credits); the GKE free-tier credit of 74.40 USD a
month is separate from the trial and stays after it.

## What changes the number

- S6, removal of the Azure estate: minus 89.8 USD a month. That is the measured cost of `rg-conde` and
  `rg-conde-infra` over 2026-10-21 to 2026-10-27 (2.95 USD a day, Cost Management API, ActualCost), by meter:
  Container Apps 42.1 (`ca-conde`, Consumption plan, 2 replicas of 0.5 vCPU and 1 GiB always on, at 0.000034 USD per
  active vCPU-second, 0.000004 per idle vCPU-second and per GiB-second, 180,000 vCPU-seconds and 360,000 GiB-seconds
  free a month), PostgreSQL flexible server `psql-conde` 18.3 (B_Standard_B1ms at 0.0199 USD an hour, 32 GB storage
  at 0.1369 USD per GB-month, 7 days backup within the free backup allowance, zone 1, no HA), the environment's load
  balancer and public IP in `rg-conde-infra` 18.1, Virtual Network 10.8 (the private endpoint of the database),
  Azure DNS 0.4, Key Vault standard and the two Standard_LRS storage accounts under 0.1, Log Analytics 0.02 GB in 30
  days within the 5 GB free. SKUs confirmed with `az` on 2026-10-28; unit prices from the Azure Retail Prices API
  (https://prices.azure.com/api/retail/prices, region westeurope). The two Container Apps jobs (`caj-conde-psql`,
  `caj-conde-dump`) cost only when run.
- The workspace count: each further workspace at the chart defaults adds 30.6 USD (two pods) plus a database and
  secrets that round to nothing; the sidecars are 4.8 USD of that per workspace. A workspace on one replica, as demo
  and clearscale run, adds 15.3 USD.
- The Secret Manager rotation poll: 2.54 USD for reads nobody consumes between rotations; a longer rotation
  interval or rotation off removes it.

## Not estimated

- GCP billing so far: the Cloud Billing API is not enabled on the gcloud quota project and the billing reports are
  console-only, so no invoice line was read. Follow-up: the export to BigQuery has no Terraform resource; it is
  switched on in the console under Billing, Billing export, Standard usage cost, with a BigQuery dataset in
  `conde-prod`. Not set up here.
- kube-system pods that are not DaemonSets (kube-dns x2, metrics-server, konnectivity-agent x2, the autoscalers,
  event-exporter, l7-default-backend, filestore-lock-release-controller, egress-nat-controller, gmp-operator):
  together 0.36 vCPU and 0.59 GiB, 17.6 USD a month if GKE bills them. The price page says only
  "You are not charged for unmodified system DaemonSet Pods, operating system overhead, unallocated space, or
  unscheduled Pods"; my reading is that these GKE-managed pods are not billed either, and the first invoice settles
  it. The `gke-system-balloon-pod` (1.995 vCPU, 18.3 GiB) is a GKE placeholder and is not counted.
- Cloud SQL backup storage: the API reports no size for the one backup; the 0.10 USD row is a 1 GiB assumption.
- Release download egress from `conde-cloud-dist`: 0.12 USD per GiB; no download count exists yet.
- The dbinit Jobs (250 m, 256 Mi, 100 Mi) run for seconds per workspace creation and round to nothing.
- Cloud Armor's Adaptive Protection (`layer_7_ddos_defense_config` enabled): the price page lists it only under the
  Enterprise tier; no Standard-tier price exists to apply.
- Cloud Storage and Artifact Registry operations, PSC and NAT data: each under 0.20 USD a month at the measured
  volumes and folded into the rows above.
