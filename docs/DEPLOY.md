# conde cloud deploy (Azure Container Apps, owned by dakai-helper; live since 2026-10-09 01:29 CEST)

- URL: https://conde.nativeai.space (managed certificate). Resource group rg-conde,
  subscription 60df47fc-ce8f-4f52-a970-43a412d78007, West Europe.
- Server: Container App ca-conde (environment cae-conde), public HTTPS + WS
  ingress, 2 replicas, 0.5 vCPU / 1 GiB. Azure FQDN
  ca-conde.nicepond-1a6fc04a.westeurope.azurecontainerapps.io.
- Postgres: psql-conde.postgres.database.azure.com (PG 18, private endpoint,
  reachable only from the app), database conde, user conde. DATABASE_URL on the
  app uses sslmode=verify-full with sslrootcert=/etc/ssl/certs/ca-certificates.crt,
  so the image must carry ca-certificates.
- Env on the app: PORT (80 on the placeholder, 8080 once conde is rolled),
  RUST_LOG=info, DATABASE_URL, CONDE_ADMIN_KEY, CONDE_DIST_BASE_URL=
  https://stconde.blob.core.windows.net/dist, ENTRA_TENANT_ID=
  2e51b484-b008-4fc0-8f70-555c1317b555, ENTRA_CLIENT_ID=
  d87a5254-3121-4d3b-bd1e-b902b3bcbba0, ENTRA_CLIENT_SECRET, ENTRA_REDIRECT_URI=
  https://conde.nativeai.space/auth/callback. Issuer
  https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0.
  Loopback redirects http://localhost/auth/cli and http://127.0.0.1/auth/cli are
  registered. Assignment required: Krisztián and Dávid.
- Secrets live in Key Vault kv-conde (database-url, admin-key,
  entra-client-secret, db-password); read at the moment of use, never stored:
  `az keyvault secret show --vault-name kv-conde --name admin-key --query value -o tsv`.

## Build and roll the server image

    az acr build --subscription 60df47fc-ce8f-4f52-a970-43a412d78007 -r crnative --platform linux/amd64 -t conde:<short-sha> -f Dockerfile .

The image carries the web app: the Dockerfile's `web` stage (node 22) runs `npm ci` and
`npm run build` in web/, and the Rust stage embeds the result. Signed-in browsers get it at
https://conde.nativeai.space/ (the keys page stays at /keys).

Then tell dakai-helper the tag; it runs
`terraform apply -var image=crnative.azurecr.io/conde:<tag> -var target_port=8080`
(one revision switch, about a minute). Logs:
`az containerapp logs show -g rg-conde -n ca-conde --type console --tail 100`.
Image contract: linux/amd64, non-root, ca-certificates, 0.0.0.0:$PORT, GET /healthz.

## Release binaries (GitHub Actions, .github/workflows/release.yml)

Repository variables AZURE_CLIENT_ID, AZURE_TENANT_ID, AZURE_SUBSCRIPTION_ID,
DIST_ACCOUNT=stconde are set; the `release` environment carries the OIDC
federated credential. Upload goes to blob container `dist` (anonymous GET, no
listing) at https://stconde.blob.core.windows.net/dist; the server redirects
/install.sh and /dist/<file> there.

## Infra decisions taken by dakai-helper
Two replicas (min = max = 2) since 2026-10-09; Postgres B1ms single zone,
no HA; no NAT gateway; Entra client secret valid until 2028-10-08. Infra code
lives in infra/ locally (not pushed; owner decides).

## Binding an admin-created user to an Entra identity

A user created by the admin API or the import (for example the owner at cutover) has no
Entra identity. If that person signs in BEFORE the admin binds them, the login creates a
second user from the UPN (`krisztian-dobmayer` on 2026-10-09) and the owner view is empty.
Bind first: `POST /v1/admin/users {"tid","oid","name":"<existing user>"}` (admin bearer key
from the vault; oid = `az ad signed-in-user show --query id`). If the sign-in already
happened, move the binding in one transaction on the database (through the job, never from
a workstation): `UPDATE identities SET user_id = <existing user id> WHERE tid = ... AND oid =
...;` then `DELETE FROM web_sessions WHERE user_id = <stray user id>;` and have the person
sign in again. Leave the stray user row.
