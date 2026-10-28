# conde cluster layer on GCP

The Kubernetes objects every workspace shares: the Gateway (`gateway.tf`), Keycloak (`keycloak.tf`) and Argo CD
(`argocd.tf`). Applied locally as terraform@conde-prod, from a machine on the admin allowlist:

    export GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com
    cd infra/gcp/cluster && terraform init && terraform apply

On an empty cluster the first apply runs in three steps, because the repository key must exist before the Job that
copies it, and `kubernetes_manifest` needs the Argo CD CRDs at plan time:

    terraform apply -target=google_secret_manager_secret.argocd_repo_ssh_key
    # add the deploy key (Repository access below)
    terraform apply -target=helm_release.argocd -target=kubernetes_job_v1.argocd_repo_import
    terraform apply

## Argo CD

https://argocd.conde.cloud, chart `argo-cd` 10.10.3 (Argo CD v3.5.5) from https://argoproj.github.io/argo-helm. The
server runs without TLS behind the Gateway, which terminates HTTPS with the wildcard certificate. Cloud Armor rule
1003 of `conde-web` (`infra/gcp/platform/edge.tf`) refuses every request for the host from outside `admin_cidrs`:
UI, API and CLI alike. The CLI goes through the load balancer with gRPC-Web:

    argocd login argocd.conde.cloud --grpc-web --sso

Sign-in is OIDC against the Keycloak realm `infra` (`infra/gcp/realms`), public client `argocd` with PKCE, no client
secret and no Dex. Access is the `groups` claim: `infra-admins` is `role:admin`, and everyone else, signed in or not,
has no permission (`policy.default` is empty).

The local `admin` account stays enabled until sign-in through the realm is verified. Its first password is in the
Secret Argo CD generates:

    kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath='{.data.password}' | base64 -d

Once an `infra-admins` member has signed in and synced an Application through SSO, set `argocd_local_admin` to false
in `variables.tf`, apply, and delete `argocd-initial-admin-secret`.

When Keycloak is down, the way in is GCP IAM: terraform@conde-prod (or any principal with Kubernetes Engine
permissions) reaches the API server through the DNS endpoint, and `argocd --core` drives Argo CD with that kubeconfig
and no Argo CD login, for example `argocd --core app sync ws-demo` after `kubectl config set-context --current
--namespace argocd`.

The namespace `argocd` is deny-by-default (`kubernetes_network_policy_v1.argocd`): in from its own pods and from the
load balancer to the server; out to its own pods, DNS, the API server, GitHub's address ranges on 22 and 443, and the
Gateway address, through which the server reaches the `infra` realm.

## Repository access

Argo CD reads github.com/kdobmayer/conde with a read-only deploy key (`argocd-conde-prod`). The private key lives in
Secret Manager as `argocd-repo-ssh-key`; the Job `argocd-repo-import` mounts that one secret through the Secret
Manager CSI add-on (its Workload Identity principal can read nothing else), builds the repository Secret `repo-conde`
with kustomize and writes it by server-side apply, so no last-applied annotation copies the key. Its role can create
Secrets and read and patch only `repo-conde`; its log is the object name. GitHub's SSH host keys, as published at
https://api.github.com/meta, are in `argocd-ssh-known-hosts-cm` through the chart values.

To rotate the key:

    D=$(mktemp -d) && ssh-keygen -q -t ed25519 -N '' -C argocd-conde-prod -f "$D/id"
    gh repo deploy-key add "$D/id.pub" -R kdobmayer/conde --title argocd-conde-prod
    gcloud secrets versions add argocd-repo-ssh-key --data-file=- --project conde-prod \
      --impersonate-service-account terraform@conde-prod.iam.gserviceaccount.com < "$D/id"
    rm -r "$D"
    terraform apply -replace=kubernetes_job_v1.argocd_repo_import
    argocd repo list    # STATUS Successful
    gh repo deploy-key list -R kdobmayer/conde && gh repo deploy-key delete <old id> -R kdobmayer/conde

## Kubernetes Secrets that hold values

Everywhere else in this estate a secret is a Secret Manager file mounted by the CSI add-on, and no Kubernetes Secret
carries a value. Argo CD reads its own credentials from Kubernetes Secrets by design, so the namespace `argocd` is the
exception, and holds exactly these:

| Secret | keys | written by |
|---|---|---|
| `argocd-secret` | `server.secretkey` (signs Argo CD session tokens), `admin.password` (bcrypt hash), `admin.passwordMtime` | the chart creates it empty; the server fills it at first start |
| `argocd-initial-admin-secret` | `password` (the local admin's first password) | the server at first start; delete it once the local admin is off |
| `argocd-redis` | `auth` (the Redis password between the Argo CD components) | the chart's `argocd-redis-secret-init` Job |
| `repo-conde` | `sshPrivateKey` (the deploy key), `url`, `type`, `name` | the `argocd-repo-import` Job, from Secret Manager |

There is no TLS material: the server runs without TLS behind the Gateway, and the repo server's internal certificate
is generated in memory. The Helm release record (`sh.helm.release.v1.argocd.*`) holds the chart values, which carry
no secret.

## Workspaces

Terraform applies the AppProject `conde` (`deploy/argocd/project.yaml`: this repository only, `ws-*` namespaces only,
only the kinds the chart renders) and the ApplicationSet `conde-workspaces` (`deploy/argocd/workspaces.yaml.tftpl`).
The ApplicationSet makes one Application `ws-<name>` per `deploy/tenants/<name>.yaml` on the branch
`gitops_revision` (default `main`) and renders `charts/conde-workspace` with three layers of values: the tenant file,
`deploy/image.yaml` (written by the release workflow), and the platform values from this root (project, Cloud SQL,
Gateway, Cloud Armor policy, egress list), which take precedence, so a tenant file cannot change them.

A git files generator rather than a list in Terraform: adding or removing a workspace is a commit to
`deploy/tenants/`, without a Terraform apply, and the tenant file is the chart's values file as it is. A new workspace
still needs its GCP side first (`infra/gcp/workspaces`, `infra/gcp/realms`).

`preserveResourcesOnDeletion` is on: deleting a tenant file or the ApplicationSet leaves the workspace running.
Pruning is off. Automated sync is `workspaces_auto_sync`, off until the workspaces adopted from Terraform have been
synced once by hand, then on with self-heal.
