# conde-workspace

One conde server for one workspace (board): the Deployment, its Service, a Gateway API route, a PodDisruptionBudget
and a deny-by-default NetworkPolicy. Install one release per workspace, each in its own namespace; the object names
are fixed (`conde`, `dbinit`), so two workspaces never share a namespace.

On conde.cloud, Argo CD renders it once per `deploy/tenants/<name>.yaml` (see `deploy/argocd/workspaces.yaml.tftpl`
and `infra/gcp/cluster/README.md`). Elsewhere it installs with Helm 3:

    helm install conde charts/conde-workspace -n ws-acme --create-namespace -f acme.yaml

## Two modes

`gcp.enabled: true` (GKE): the server reaches Cloud SQL through the Cloud SQL Auth Proxy as a native sidecar with
automatic IAM authentication, so the database URL holds no password; its secrets are Secret Manager files mounted by
the GKE Secret Manager CSI add-on under `/secrets` (no Kubernetes Secret); the GKE load balancer gets a
HealthCheckPolicy and a GCPBackendPolicy with the Cloud Armor policy; and the `dbinit` Job, once, as the `postgres`
user, hands the database and its public schema to the workspace's IAM user. The Google side (service account, IAM
user, grants, secrets) comes from `infra/gcp/modules/workspace`.

`gcp.enabled: false` (any cluster): the container's environment comes from one Secret you provide, either
`existingSecret` (you create it) or `externalSecret.enabled` (External Secrets Operator creates `<release>-env` from
your store). The Secret holds:

| key | value |
|---|---|
| `DATABASE_URL` | `postgresql://user:password@host:5432/db` of a database the user owns |
| `CONDE_ADMIN_KEY` | the admin API key, 32 or more random characters |
| `OIDC_CLIENT_SECRET` | the client secret of the server's OIDC client |
| `CONDE_SLACK_BOT_TOKEN` | optional, the Slack bot token (`chat:write`) |

## Values

| value | default | meaning |
|---|---|---|
| `name` | required | workspace name: the `conde.cloud/workspace` label, the default board and database name |
| `hostname` | required | public hostname; the OIDC redirect is `https://<hostname>/auth/callback` |
| `boardName` | `name` | board name shown to users |
| `replicas` | `2` | server replicas |
| `image.repository`, `image.tag` | the conde Artifact Registry image, `gcp` | server image |
| `image.digest` | `""` | when set, the image is `repository@digest` and the tag is ignored; on conde.cloud the release workflow sets it in `deploy/image.yaml` |
| `image.pullPolicy` | unset | container pull policy |
| `oidc.issuer` | required | OIDC issuer the server signs in against |
| `oidc.clientId` | `conde` | OIDC client id |
| `oidc.redirectUri` | `https://<hostname>/auth/callback` | OIDC redirect URI |
| `logLevel` | `info` | `RUST_LOG` |
| `distBaseUrl` | the conde.cloud dist bucket | where `conde update` downloads binaries |
| `extraEnv` | `[]` | extra container env entries |
| `resources` | 250m CPU, 512Mi memory, 1Gi ephemeral storage | server container resources |
| `terminationGracePeriodSeconds` | `600` | time for SSE and WebSocket streams to close |
| `tolerations` | the amd64 toleration GKE Autopilot adds | pod tolerations; `[]` elsewhere |
| `namespace.create`, `namespace.labels` | `false`, the shared-Gateway label | render the Namespace object (Argo CD does; leave false with `--create-namespace`) |
| `route.enabled`, `route.gateway.name/namespace/sectionName` | `true`, `conde`/`gateway`/`https` | the HTTPRoute and its parent Gateway listener |
| `networkPolicy.enabled` | `true` | deny-by-default NetworkPolicy |
| `networkPolicy.ingressCidrs` | Google load balancer ranges | sources allowed to port 8080 |
| `networkPolicy.egress` | `[]` | egress besides kube-dns: `{cidr, except, ports: [{protocol, port}]}` |
| `existingSecret` | `""` | without `gcp.enabled`: the Secret the env comes from |
| `externalSecret.enabled`, `.secretStoreRef`, `.data`, `.refreshInterval` | off | without `gcp.enabled`: an ExternalSecret mapping env names to remote keys |
| `gcp.enabled` | `false` | the GKE mode above |
| `gcp.project` | required with gcp | project of the Secret Manager secrets |
| `gcp.serviceAccount` | required with gcp | the workspace's Google service account email (Workload Identity, IAM database user) |
| `gcp.sqlConnectionName` | required with gcp | Cloud SQL instance connection name |
| `gcp.cloudSqlProxyImage` | pinned Cloud SQL Auth Proxy | sidecar image |
| `gcp.database` | `conde_<name>` | database name |
| `gcp.secrets.adminKey`, `.oidcClientSecret`, `.slackBotToken` | required with gcp | Secret Manager secret ids mounted as `admin-key`, `oidc-client-secret`, `slack-bot-token` |
| `gcp.securityPolicy` | `""` | Cloud Armor policy on the backend |
| `gcp.backendTimeoutSec` | `3600` | load balancer backend timeout (SSE and WebSocket streams) |
| `gcp.dbinit.enabled`, `.image`, `.adminSecret` | `true`, pinned postgres, `pg-admin-password` | the database hand-over Job and the secret with the `postgres` password |

The Namespace and the dbinit objects carry `argocd.argoproj.io/sync-wave: "-1"`, so Argo CD finishes the database
hand-over before it starts the server; Helm ignores the annotation.

The selector labels are `app.kubernetes.io/name: conde` and `conde.cloud/workspace: <name>` and never change for a
release; the chart adds no other labels to the pod template, so a values change that does not touch the pod leaves
the running pods alone.
