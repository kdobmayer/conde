# Keycloak realms of conde on GCP

The configuration of the Keycloak at https://auth.conde.cloud: the realms `demo` (local users with passwords) and
`clearscale` (Google sign-in only, for clearscale.com accounts), one conde workspace each, while `dakai` signs in
directly against its Entra tenant; the realm `infra` for platform tools (Argo CD, group `infra-admins`, MFA
required); and in the master realm the permanent admin `admin` and the client `terraform`.
Apply after the cluster and workspaces roots, from a machine on the admin allowlist: the provider calls `/admin`
and `/realms/master`, which Cloud Armor refuses from anywhere else.

    export GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com
    export CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT=$GOOGLE_IMPERSONATE_SERVICE_ACCOUNT
    export KEYCLOAK_CLIENT_ID=terraform
    export KEYCLOAK_CLIENT_SECRET="$(gcloud secrets versions access latest --secret keycloak-terraform-client-secret --project conde-prod)"
    cd infra/gcp/realms && terraform init && terraform apply

The first apply, before the `terraform` client exists, signs in as the bootstrap admin instead:

    export KEYCLOAK_CLIENT_ID=admin-cli KEYCLOAK_USER=bootstrap-admin
    export KEYCLOAK_PASSWORD="$(gcloud secrets versions access latest --secret keycloak-bootstrap-admin --project conde-prod)"

## Secrets

Every generated value is write-only: it is in Keycloak and in Secret Manager, not in this state.

| secret | value |
|---|---|
| `demo-oidc-client-secret`, `clearscale-oidc-client-secret` | the secret of the realm's `conde` client (a new version of the secret the workspaces root created) |
| `keycloak-admin-temp-password` | the temporary password of `admin` in the master realm |
| `infra-krisztian-temp-password` | the temporary password of `krisztian` in the realm `infra` |
| `keycloak-terraform-client-secret` | the secret of the master realm's `terraform` client |

A conde server reads its client secret at start: after a new version, `kubectl -n ws-<name> rollout restart
deployment/conde`. Two secrets this root reads are in its state, because their provider fields are not write-only:
`resend-api-key` (the SMTP password of every realm) and `<name>-google-oauth-client-secret`.

## Replacing the bootstrap admin

1. Read the temporary password: `gcloud secrets versions access latest --secret keycloak-admin-temp-password
   --project conde-prod`. Sign in at https://auth.conde.cloud/admin/master/console/ as `admin`; Keycloak asks for a
   new password and an authenticator app (OTP). Sign out, then sign in again with the new password and an OTP code.
2. Switch Terraform to the `terraform` client (the first block above, without `KEYCLOAK_USER` and
   `KEYCLOAK_PASSWORD`: `unset KEYCLOAK_USER KEYCLOAK_PASSWORD`) and run `terraform plan`: it must sign in and show
   no changes.
3. In the admin console as `admin`: realm master, Users, `bootstrap-admin`, Delete. Keycloak creates the bootstrap
   user only while the master realm has no user at all, so it does not come back on a restart.
4. In the cluster root, drop `KC_BOOTSTRAP_ADMIN_USERNAME`, the `bootstrap-admin-password` mount and the
   `keycloak-bootstrap-admin` secret; they are unused from step 3 on.

## Later switches

- MFA for a password realm: `require_mfa = { demo = true, clearscale = false }` makes Configure OTP a default
  action; Keycloak assigns it to users created after the change, so existing users get it by hand (Users, the user,
  Required user actions). A Google realm has no password form; its MFA is the company's Google Workspace policy.
- Local passwords back on in a Google realm: in its `google browser` flow, set the `google browser forms` subflow to
  ALTERNATIVE and the password form in it to REQUIRED (`keycloak_authentication_subflow.google_forms` and
  `keycloak_authentication_execution.google_password`). The login page then shows both the form and the Google button.
- Email: every realm, master included, sends through Resend (`local.smtp` in main.tf: smtp.resend.com, SMTPS on 465,
  user `resend`, the API key from `resend-api-key`), from `no-reply@conde.cloud` with replies to the owner. Keycloak
  needs egress to TCP 465, an egress rule of its NetworkPolicy in the cluster root. "Forgot password" is on in the realms
  that sign in with passwords (`passwords` in `local.realms`): demo.

## Adding a company

Each company is one workspace (`infra/gcp/workspaces`, the `local.workspaces` map), which creates its namespace, its
database and the empty secret `<name>-oidc-client-secret`. How its people sign in decides the rest.

**A company on Google Workspace** gets a realm here that signs in only through Google:

1. The workspace, without `oidc_issuer` (the server then uses the realm `https://auth.conde.cloud/realms/<name>`),
   and `/realms/<name>` in the Keycloak HTTPRoute of the cluster root; the load balancer routes no other realm path.
2. An OAuth client in the Google Cloud console of conde-prod (APIs and services, Credentials, OAuth client ID, Web
   application) with the authorised redirect URI `https://auth.conde.cloud/realms/<name>/broker/google/endpoint`.
   Its secret goes into Secret Manager:

       gcloud secrets create <name>-google-oauth-client-secret --replication-policy=user-managed \
         --locations=europe-west3 --project conde-prod
       printf '%s' "$SECRET" | gcloud secrets versions add <name>-google-oauth-client-secret --data-file=- --project conde-prod

3. Here, one entry in each of `local.tenants` (display name, hostname), `var.require_mfa` (false) and
   `local.google_sign_in` (the company's Workspace domain as `hosted_domain`, the OAuth client id). The entry in
   `google_sign_in` gives the realm the Google identity provider and the `google browser` flow, so every sign-in goes
   straight to Google and Keycloak refuses accounts of other domains. Then `terraform apply`; it writes the realm's
   `conde` client secret into `<name>-oidc-client-secret`.

**A company on Microsoft Entra** gets no realm: its workspace signs in directly against its tenant, as dakai does.

1. An app registration in the company's tenant (made by its admin; dakai's is `infra/terraform/entra.tf`): web
   redirect URI `https://<name>.conde.cloud/auth/callback`, logout URL `https://<name>.conde.cloud/`, the Microsoft
   Graph delegated permissions `openid`, `profile` and `email`, and a client secret.
2. The workspace with `oidc_issuer = "https://login.microsoftonline.com/<tenant id>/v2.0"` and `oidc_client_id` = the
   registration's application id, and the client secret added by hand to `<name>-oidc-client-secret` (the workspaces
   README has the command). Nothing changes in this root.
