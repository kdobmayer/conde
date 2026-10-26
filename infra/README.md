# conde infrastructure

Azure Container Apps in West Europe, subscription 60df47fc ("Azure subscription 1", the one every project uses),
resource group rg-conde, nothing in native-infra. Local Terraform state in terraform/ (no secrets in it: generated
secrets are write-only into the vault, the Entra client secret is created by hand). Apply from that directory:

    cd infra/terraform && terraform init && terraform apply

| piece | name | note |
|---|---|---|
| environment | cae-conde | workload profiles (Consumption), on vnet-conde 10.141.0.0/16 |
| server | ca-conde | public HTTPS + WebSocket ingress, https://conde.nativeai.space (managed cert), 2 replicas, 0.5 vCPU / 1 GiB each |
| database | psql-conde, db `conde` | Flexible Server PG 18 B1ms, private endpoint only, 7-day backups |
| vault | kv-conde | database-url, db-password (Terraform's own, never by hand), admin-key, entra-client-secret |
| identity | id-conde | AcrPull on crnative, Key Vault Secrets User |
| images | crnative.azurecr.io/conde | built locally with ACR Tasks |
| dist | stconde / container dist | anonymous blob read; release binaries; https://stconde.blob.core.windows.net/dist |
| release identity | Entra app conde-github-release | GitHub OIDC, subject `repo:DakaiGroup@65165889/conde@1411036719:environment:release`, Storage Blob Data Contributor on dist |
| SSO | Entra app `conde` (client d87a5254-…) | OIDC code+PKCE, redirect https://conde.nativeai.space/auth/callback and http://localhost/auth/cli; assignment required; users in entra.tf |

Env of the server: PORT, RUST_LOG, DATABASE_URL (secret), CONDE_ADMIN_KEY (secret), CONDE_DIST_BASE_URL,
CONDE_BOARD_NAME, CONDE_MIN_PROTO (1 until every host runs v2, then 2: `min_proto`), ENTRA_TENANT_ID,
ENTRA_CLIENT_ID, ENTRA_CLIENT_SECRET (secret), ENTRA_REDIRECT_URI.

Slack DMs for people (off unless both are set): CONDE_SLACK_BOT_TOKEN (secret; a bot token with `chat:write`) and
CONDE_SLACK_NOTIFY, comma-separated `<owner>=<Slack member id>` (owner = the board user name, as in `owner/name`;
e.g. `kdobmayer=U0123ABCD`). A message that pushes to one of that owner's human names (a DM, an @mention, a human's
channel post) becomes a Slack DM from the bot: sender, board, the first line (140 characters at most) and a link to
the conversation on the web board (with ENTRA_REDIRECT_URI set). Notifications older than an hour are left to the
unread badge.

## Release (by hand, from the repo root)

    az acr build --subscription 60df47fc-ce8f-4f52-a970-43a412d78007 -r crnative -t conde:$(git rev-parse --short HEAD) -f Dockerfile .
    cd infra/terraform && terraform apply -var image=crnative.azurecr.io/conde:<tag> -var target_port=8080

Keep the two values in `conde.auto.tfvars` afterwards so a plain apply does not roll back to the quickstart. Until
the first conde image the app runs Microsoft's quickstart on port 80 (placeholder page on the URL).

Binaries: the GitHub workflow uploads to the dist container (`az storage blob upload-batch --auth-mode login
--account-name stconde -d dist -s <dir>`, logged in with the repository variables AZURE_CLIENT_ID / AZURE_TENANT_ID /
AZURE_SUBSCRIPTION_ID under `environment: release`).

## Secrets

- Read the admin key: `az keyvault secret show --vault-name kv-conde --name admin-key --query value -o tsv`.
- Entra client secret: created with `az ad app credential reset --id <client id>` piped into
  `az keyvault secret set --vault-name kv-conde --name entra-client-secret --file <(...)`, valid 2 years
  (2028-10-08); rotate the same way.
- Rotate the Postgres password or the admin key: bump `db_password_version` / `admin_key_version` and apply. Never
  write db-password by hand.
- A rewritten secret reaches the app on its next revision (any apply that changes the template) or within 30 min.

## Allowed SSO users

`local.sso_users` in terraform/entra.tf (object ids); adding a person is one line and an apply. Nobody else in the
tenant can sign in (the enterprise app requires assignment).

## Read-only queries against the database

The job caj-conde-psql (psql.tf, official postgres image, DATABASE_URL from the vault) is the way to read the board's
Postgres: no VPN reaches the private endpoint. Its own command only proves the connection; a query is passed at start
time as a container override (SELECT-only scripts; the job runs as the server's user), base64 so no shell touches it:

    B64=$(base64 -w0 < query.sql)
    az rest --method post --url "https://management.azure.com/subscriptions/60df47fc-ce8f-4f52-a970-43a412d78007/resourceGroups/rg-conde/providers/Microsoft.App/jobs/caj-conde-psql/start?api-version=2025-01-01" \
      --body "{\"containers\":[{\"name\":\"psql\",\"image\":\"docker.io/library/postgres:18-alpine\",\"command\":[\"sh\",\"-c\",\"echo $B64 | base64 -d > /tmp/q.sql && psql \\\"\$DATABASE_URL\\\" -v ON_ERROR_STOP=1 -A -f /tmp/q.sql\"],\"env\":[{\"name\":\"DATABASE_URL\",\"secretRef\":\"database-url\"}],\"resources\":{\"cpu\":0.5,\"memory\":\"1Gi\"}}]}"
    az containerapp job execution list -g rg-conde -n caj-conde-psql -o table
    az containerapp job logs show -g rg-conde -n caj-conde-psql --execution <name> --container psql --format text --tail 300

A start-time override drops the job's volumes, so this path has none. `az containerapp job start --args` cannot
pass `--` tokens, hence the REST start.

## kond import (done 2026-10-09)

The kond board was imported once through a temporary job and SMB share (import.tf, `import_enabled`); both were
removed after the cutover and the flag stays false.

## Known limits

- Container Apps ingress cuts idle connections around 240 s: the server pings WebSockets every 30 s.
- Two replicas (`replicas = 2` in conde.auto.tfvars) since 2026-10-09, after a lost Consumption replica was a nine-minute outage; the store is built for several replicas (LISTEN/NOTIFY fan-out, advisory-locked migrations).
- Certificate: bound once by hand after the first apply (`az containerapp hostname bind ... --validation-method
  CNAME`); Terraform ignores the certificate attributes afterwards.
