# S4: Identity-Aware Proxy in front of the admin surfaces

The Keycloak admin console and the master realm move to their own host, https://admin-auth.conde.cloud, and Argo CD
stays on https://argocd.conde.cloud; both hosts sit behind Identity-Aware Proxy (IAP), which lets only
kdobmayer.devops@gmail.com through. Then the IP allowlist (`admin_cidrs`, Cloud Armor rules 1000 and 1001 of
`conde-web`) goes, and the public host https://auth.conde.cloud serves only the workspace realms, the `infra` realm
and the theme resources, with unconditional denies for `/admin` and `/realms/master`.

## What is built

- Keycloak (`infra/gcp/cluster/keycloak.tf`): `KC_HOSTNAME_ADMIN=https://admin-auth.conde.cloud`; a second Service
  `keycloak-admin` on the same pods, so the load balancer gives it a backend service of its own; an HTTPRoute for
  admin-auth.conde.cloud with `/admin`, `/realms/master`, `/resources` and `/_gcp_gatekeeper` (IAP's sign-in return
  path); a HealthCheckPolicy; a GCPBackendPolicy with IAP and the Cloud Armor policy `conde-keycloak-admin`.
- The master realm's frontend URL is https://admin-auth.conde.cloud (`infra/gcp/realms/master.tf`), so its sign-in
  pages, and with them the console's own sign-in, are on that host. The other realms keep auth.conde.cloud as issuer.
- Argo CD (`infra/gcp/cluster/argocd.tf`): its GCPBackendPolicy gets IAP and the Cloud Armor policy `conde-argocd`.
  Argo CD's own sign-in through the `infra` realm stays the application login.
- IAP (`infra/gcp/cluster/iap.tf`): the custom OAuth client `iap-admin`; its secret written by Terraform into the
  Kubernetes Secret `iap-admin` in `keycloak` and `argocd` as write-only data from an ephemeral Secret Manager read,
  so it is in neither plan nor state (the second exception to "no Kubernetes Secret carries a value",
  infra/gcp/cluster/README.md); and `roles/iap.httpsResourceAccessor` for user:kdobmayer.devops@gmail.com on the
  two backend services only.
- Cloud Armor (`infra/gcp/platform/edge.tf`): `conde-keycloak-admin` (300 requests a minute per IP) and
  `conde-argocd` (600), throttles only; IAP decides who gets in. Cloud Armor is evaluated before IAP.
- The IAP API (`infra/gcp/foundation/main.tf`).
- The realms root reaches Keycloak through a port-forward (`infra/gcp/realms/run.sh`), so Terraform no longer needs
  the public admin paths. The Argo CD CLI goes through `kubectl port-forward -n argocd svc/argocd-server`
  (infra/gcp/cluster/README.md).
- DNS: nothing to add. The record `*.conde.cloud` already resolves admin-auth.conde.cloud to the Gateway address, and
  the certificate's `*.conde.cloud` covers it (one label; `admin.auth.conde.cloud` would not be covered).

Why a custom OAuth client: Google's managed IAP client admits only users of the organization, and the owner's
principal is a Gmail account. A client of the Google Auth Platform app `conde` with External audience admits any
Google account; the IAM grant then decides.

## a) The owner creates the OAuth client

1. https://console.cloud.google.com/auth/clients?project=conde-prod, Create client. Application type: Web
   application. Name: `iap-admin`. Create. Copy the client ID and the client secret (the secret is shown only now).
2. Open the client `iap-admin` again, Authorised redirect URIs, Add URI:
   `https://iap.googleapis.com/v1/oauth/clientIds/<client ID>:handleRedirect`, Save.
3. https://console.cloud.google.com/auth/audience?project=conde-prod: user type must be External. If the publishing
   status is Testing, add kdobmayer.devops@gmail.com under Test users.
4. In a terminal (the secret is read without echo and stays out of the shell history):

       read -rs S && printf '%s' "$S" | pass insert -m conde/iap-admin-client-secret && unset S
       pass show conde/iap-admin-client-secret | gcloud secrets create iap-admin-client-secret \
         --replication-policy=user-managed --locations=europe-west3 --data-file=- --project conde-prod

5. Send desk the client ID (it is not a secret).

## b) to e) Apply order (desk, on the owner's go)

Environment as in the READMEs: `GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com`,
`CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT` the same. Steps b to d run at the commit before "Retire the admin IP
allowlist"; the public admin paths stay open to `admin_cidrs` until step e, so there is a way back until then.

Every apply runs with `TF_LOG` unset: the Kubernetes provider logs the `iap-admin` Secret object, value included, at
INFO when it creates or updates it. Provider logs are never collected as evidence of a step; the checks below are.

b) Secrets and permissions.
   - Terraform's own identity can set IAP policies: `gcloud projects add-iam-policy-binding conde-prod
     --member=serviceAccount:terraform@conde-prod.iam.gserviceaccount.com --role=roles/iap.admin --condition=None`
     (its roles are set outside Terraform, like the others it holds).
   - `iap_client_id` in `infra/gcp/cluster/variables.tf` set to the client ID as its default.
   - foundation: `terraform apply` (the IAP API; plan: 1 to add).

c) Admin host, routes, policies with IAP.
   - platform: `terraform apply` (plan: 2 to add, the two Cloud Armor policies).
   - cluster: `terraform apply`. It writes `iap-admin` into both namespaces before the policies that name it, then
     upgrades the Argo CD release; Keycloak rolls once for `KC_HOSTNAME_ADMIN` (one pod, surge 1, no gap). Every
     cluster plan from now on reads `iap-admin-client-secret`, so it must exist (step a) before the first one.
     The Gateway controller creates the `keycloak-admin` backend service during this apply, so its IAP grant needs
     a second `terraform apply`; the check `iap_backend_services` warns until it has run. Argo CD is behind IAP
     from this apply on.
   - realms, first the rotation of the master realm's `terraform` client secret (`terraform_client_secret_version`
     is 2 on this branch; README.md: Rotating the terraform client secret): the targeted `./run.sh apply` of the
     client and its secret version (the version holding the old value is destroyed), then re-export
     `KEYCLOAK_CLIENT_SECRET` from Secret Manager. Then `./run.sh apply` (1 to change, the master realm's
     `frontendUrl`). From here the console is on admin-auth.conde.cloud.

d) Verify (every item, before step e):
   1. IAP on both backends:
      `gcloud compute backend-services list --global --format='table(name,iap.enabled,iap.oauth2ClientId,securityPolicy.basename())'`:
      the `keycloak-keycloak-admin-8080` and `argocd-argocd-server-80` rows show True, the client ID, and
      `conde-keycloak-admin` and `conde-argocd`; every other row has IAP off and `conde-web`.
   2. The grant, on each of the two: `gcloud iap web get-iam-policy --resource-type=backend-services --service=<name>`:
      only user:kdobmayer.devops@gmail.com with roles/iap.httpsResourceAccessor. Nothing project-wide:
      `gcloud iap web get-iam-policy` shows no binding.
   3. `kubectl get gcpbackendpolicy -A`: `keycloak-admin` and `argocd-server` attached, no error condition. The
      Secret's data key `key` follows Google's published examples; the CRD does not name the key, so an error here
      about the client secret points there first.
   4. Owner's account, private window: https://admin-auth.conde.cloud/admin/master/console/ goes to Google, then to
      the master realm's sign-in on admin-auth.conde.cloud (password and OTP), and the console loads. Then: open the
      realm `demo` and list its users (the admin API); reload the page (the console signs in again silently); leave
      it idle past the access token lifetime and click on (token refresh through IAP); leave it past the IAP session
      and click on (a script request cannot follow IAP's sign-in, so the console's calls fail until a reload, which
      signs in again; record what it shows); sign out (back to the master sign-in on admin-auth.conde.cloud).
   5. An unrelated Google account, private window, on both hosts: IAP refuses it ("You don't have access").
   6. Argo CD: https://argocd.conde.cloud with the owner's account goes through IAP, then "Log in via Keycloak"
      (`infra` realm on auth.conde.cloud) and back; the applications list and an application's tree load. The CLI
      through the port-forward: `argocd login 127.0.0.1:18081 --plaintext --sso`, then `argocd app list`.
   7. No other way to the admin backends: without an IAP cookie,
      `curl -sI https://admin-auth.conde.cloud/admin/master/console/` answers 302 to accounts.google.com;
      `curl -s -o /dev/null -w '%{http_code}\n' https://admin-auth.conde.cloud/realms/demo` answers 404 (not routed);
      `curl -sI -H 'Host: admin-auth.conde.cloud' https://auth.conde.cloud/admin/` answers the IAP redirect (the load
      balancer routes on the Host header, so a mismatched certificate name gives no way around IAP); the cluster's
      HTTPRoutes (`kubectl get httproute -A -o wide`) name admin-auth.conde.cloud once and argocd.conde.cloud once.
   8. Encoded paths on the public host cannot reach the admin or master paths. From an address outside `admin_cidrs`
      (Cloud Shell), each of these answers 403 and never Keycloak's master realm or console:
      `https://auth.conde.cloud/realms/demo/%2e%2e/master/.well-known/openid-configuration`,
      `https://auth.conde.cloud/realms/demo/..%2fmaster/.well-known/openid-configuration`,
      `https://auth.conde.cloud/resources/%2e%2e/admin/master/console/`
      (`curl -s -o /dev/null -w '%{http_code}\n' '<url>'`). Cloud Armor evaluates the normalised path, so rules
      1000 and 1001 are what refuse them; rule 1002 is defence in depth only.
   9. The realms root signs in through the port-forward: `./run.sh plan` shows no changes.

e) Retire the allowlist (the commit "Retire the admin IP allowlist").
   - platform: `terraform apply`: rules 1000 and 1001 of `conde-web` deny `/admin`, `/admin/*`, `/realms/master` and
     `/realms/master/*` to everyone (rule 1002 keeps refusing encoded and dot-segment paths on auth.conde.cloud);
     `admin_cidrs` is gone.
   - cluster: `terraform apply`: the public Keycloak route loses `/admin` and `/realms/master`.
   - Rule 1003 (Argo CD host from `admin_cidrs`) goes with them; it stopped applying at step c, when the Argo CD
     backend moved to `conde-argocd`.
   - Check from an address that was on the allowlist and from one that was not (Cloud Shell): on auth.conde.cloud,
     `/admin/master/console/`, `/realms/master` and `/realms/demo/%2e%2e/master/` are refused (403 or 404);
     `/realms/demo/.well-known/openid-configuration` and `/realms/infra/.well-known/openid-configuration` answer 200;
     a demo sign-in and an Argo CD sign-in still work.

## Notes

- Keycloak 26.8.1 serves no `/js` path (the root resources are `/`, `/admin`, `/realms`, `/resources`,
  `/.well-known`, `/lb-check` and `/welcome-content`), so neither host routes it.
- The console builds some links from its own server URL, which is now admin-auth.conde.cloud even for a tenant realm:
  a client's Home URL and an identity provider's redirect URI shown in the console point at the admin host. The
  redirect URI to register at Google for a company realm stays
  `https://auth.conde.cloud/realms/<name>/broker/google/endpoint` (infra/gcp/realms/README.md).
- Rotation of the IAP client secret: add a version to `iap-admin-client-secret`, set `iap_client_secret_version`
  to its number, apply the cluster root with `TF_LOG` unset (infra/gcp/cluster/README.md, Kubernetes Secrets that hold
  values). The Secrets hold exactly that version; a new "latest" alone changes nothing.

Sources read for this design: Keycloak 26.8.1 `HostnameV2Provider.java` (the realm's `frontendUrl` overrides the
frontend URL; the admin URL is `hostname-admin`), `AdminConsole.java` (the console's sign-in server is the realm's
frontend URL, its API base the admin URL), `AdminRoot.java` and `DefaultKeycloakContext.java` (an admin API token is
checked against its realm's frontend URL, whatever host it arrives on), the admin UI's `KeycloakContext.tsx` and
`i18n.ts` (sign-in on `/realms/master`, messages from `/resources`), and the server guide `hostname.adoc`
(`hostname-admin` does not close the admin API on the frontend host; that is the proxy's job). GKE: the
GCPBackendPolicy CRD in the cluster (a custom OAuth client needs both `clientID` and `oauth2ClientSecret`; neither
means Google's managed client) and docs.cloud.google.com/kubernetes-engine/docs/how-to/configure-gateway-resources
(one policy per Service, in the Service's namespace). IAP: docs.cloud.google.com/iap/docs/sessions-howto (401 to
script requests after the session ends) and docs.cloud.google.com/iap/docs/faq (identical OAuth client on every
IAP backend of a load balancer).
