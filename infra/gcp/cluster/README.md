# conde cluster layer on GCP

The Kubernetes objects every workspace shares: the Gateway (`gateway.tf`), Keycloak (`keycloak.tf`) and Argo CD
(`argocd.tf`), and Identity-Aware Proxy in front of the admin backends (`iap.tf`). Applied locally as
terraform@conde-prod, which reaches the API server on the cluster's DNS endpoint:

    export GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com
    cd infra/gcp/cluster && terraform init && terraform apply

`iap_client_id` (`variables.tf`) is the client id of the OAuth client `iap-admin`. A new admin backend gets its
backend service from the Gateway controller during the apply, after Terraform has read the Gateway, so its IAP grant
lands in a second apply; until then the check `iap_backend_services` warns.

On an empty cluster the first apply runs in three steps, because the repository key must exist before the hook that
copies it, and `kubernetes_manifest` needs the Argo CD CRDs at plan time:

    terraform apply -target=google_secret_manager_secret.argocd_repo_ssh_key
    # add the deploy key (Repository access below)
    terraform apply -target=helm_release.argocd
    terraform apply

## Argo CD

https://argocd.conde.cloud, chart `argo-cd` 10.10.3 (Argo CD v3.5.5) from https://argoproj.github.io/argo-helm. The
server runs without TLS behind the Gateway, which terminates HTTPS with the wildcard certificate. The whole host is
behind Identity-Aware Proxy (`iap.tf`): only kdobmayer.devops@gmail.com passes, then signs in to Argo CD itself; the
Cloud Armor policy `conde-argocd` throttles each client IP. The CLI cannot answer IAP's browser sign-in, so it goes
through a port-forward to the server instead of the load balancer:

    kubectl port-forward -n argocd svc/argocd-server 18081:80 --address=127.0.0.1 &
    argocd login 127.0.0.1:18081 --plaintext --sso

(IAP also admits a program that sends a Google-signed ID token in `Proxy-Authorization`; not set up.)

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
Secret Manager as `argocd-repo-ssh-key`. The Job `argocd-repo-import-hook`, a post-install and post-upgrade hook of
the Argo CD release, mounts that one secret through the Secret Manager CSI add-on (its Workload Identity principal can
read nothing else), builds the repository Secret `repo-conde` with kustomize and writes it by server-side apply, so no
last-applied annotation copies the key. Its role can create Secrets and read and patch only `repo-conde`; its log is
the object name. Ten minutes after it finishes, the TTL controller deletes the Job and its pod, and with the pod its
service account token. GitHub's SSH host keys, as published at
https://api.github.com/meta, are in `argocd-ssh-known-hosts-cm` through the chart values.

To rotate the key:

    D=$(mktemp -d) && ssh-keygen -q -t ed25519 -N '' -C argocd-conde-prod -f "$D/id"
    gh repo deploy-key add "$D/id.pub" -R kdobmayer/conde --title argocd-conde-prod
    gcloud secrets versions add argocd-repo-ssh-key --data-file=- --project conde-prod \
      --impersonate-service-account terraform@conde-prod.iam.gserviceaccount.com < "$D/id"
    rm -r "$D"
    # raise argocd_repo_key_version in variables.tf by one, then:
    terraform apply                                       # the release upgrades and the hook re-imports the key
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
| `repo-conde` | `sshPrivateKey` (the deploy key), `url`, `type`, `name` | the `argocd-repo-import-hook` Job, from Secret Manager |
| `iap-admin` | `key` (the IAP OAuth client secret) | Terraform (`iap.tf`), write-only, from Secret Manager |

The second exception is the IAP OAuth client secret: GCPBackendPolicy reads it only from a Kubernetes Secret in the
backend's namespace, so `iap-admin` exists in `argocd` and in `keycloak`. Terraform reads `iap-admin-client-secret`
from Secret Manager as an ephemeral value and writes it as the Secret's write-only data (`data_wo`), so the value is in
neither the plan nor the state; the Secret object itself is the only copy outside Secret Manager. To rotate: add a
version to `iap-admin-client-secret`, raise `iap_client_secret_version` by one, apply.

## Workspaces

Terraform applies the AppProject `conde` (`deploy/argocd/project.yaml`: this repository only, `ws-*` namespaces only,
Namespace objects named `ws-*` only, only the kinds the chart renders) and the ApplicationSet `conde-workspaces`
(`deploy/argocd/workspaces.yaml.tftpl`). The ApplicationSet makes one Application `ws-<name>` per
`deploy/tenants/<name>.yaml` on the branch `gitops_revision` (default `main`) and renders `charts/conde-workspace`
from three sources:

- the file name `<name>`: the workspace name, `<name>.conde.cloud`, the Google service account, the database
  `conde_<name>`, the Secret Manager secrets, the dbinit image and admin secret, and the network policy switch, all
  as Helm parameters, which outrank every other source;
- the tenant file, of which only `boardName`, `oidc.issuer`, `oidc.clientId` and the optional `replicas` (default 2;
  `replicas: 0` stops the server, as for a database restore) are read (it is not a values file, so nothing else in it
  reaches the chart);
- `deploy/image.yaml`, written by the release workflow, and the platform values from this root (project, Cloud SQL,
  Gateway, Cloud Armor policy, egress list).

A git files generator rather than a list in Terraform: adding or removing a workspace is a commit to
`deploy/tenants/`, without a Terraform apply. A new workspace still needs its GCP side first (`infra/gcp/workspaces`,
`infra/gcp/realms`).

`preserveResourcesOnDeletion` is on: deleting a tenant file or the ApplicationSet leaves the workspace running.
Sync is automated with self-heal, so a change made by hand to a workspace object is reverted to git; a new image goes
in through `deploy/image.yaml`, not `kubectl set image`. Pruning is off: an object dropped from the chart stays until
it is deleted by hand. `workspaces_auto_sync = false` pauses automation for every workspace (`automated.enabled:
false`), as for the first adoption of the Terraform-created objects; a targeted apply of
`kubernetes_manifest.argocd_workspaces` switches it either way in about 15 seconds.
