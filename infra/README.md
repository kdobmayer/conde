# conde infrastructure

Azure Container Apps in West Europe, subscription 60df47fc ("Azure subscription 1", the one every project uses),
resource group rg-conde, nothing in native-infra. Local Terraform state in terraform/ (no secrets in it: generated
secrets are write-only into the vault, the Entra client secret is created by hand). Apply from that directory:

    cd infra/terraform && terraform init && terraform apply

| piece | name | note |
|---|---|---|
| environment | cae-conde | workload profiles (Consumption), on vnet-conde 10.141.0.0/16 |
| server | ca-conde | public HTTPS + WebSocket ingress, https://conde.nativeai.space (managed cert), 1 replica, 0.5 vCPU / 1 GiB |
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

## kond import (one-off, cutover)

The job caj-conde-import runs the conde image against the cloud Postgres with the kond snapshot mounted read-only
at /mnt/kond from the SMB share kond-snapshot (storage account stcondeimport, temporary: `import_enabled = false`
removes job, share and account after the cutover).

    # upload the snapshot (owner identity, no key)
    az storage file upload-batch --auth-mode login --account-name stcondeimport --destination kond-snapshot --source <snapshot dir>
    # dry run (the job's default args), then the real run with overridden args
    az containerapp job start -g rg-conde -n caj-conde-import
    az containerapp job start -g rg-conde -n caj-conde-import --args import --from /mnt/kond --owner kdobmayer --host homepc
    az containerapp job execution list -g rg-conde -n caj-conde-import -o table
    az containerapp logs show -g rg-conde -n caj-conde-import --type console --tail 200

## Known limits

- Container Apps ingress cuts idle connections around 240 s: the server pings WebSockets every 30 s.
- One replica today; going to two needs the app stateless (it is designed so) and `replicas = 2`.
- Certificate: bound once by hand after the first apply (`az containerapp hostname bind ... --validation-method
  CNAME`); Terraform ignores the certificate attributes afterwards.
