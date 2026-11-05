# Dakai move back to Azure: GKE ws-dakai to ca-conde, same address

The Dakai board moves from the GKE workspace `ws-dakai` (database `conde_dakai` on Cloud SQL `conde-pg`) back to the
Azure Container App `ca-conde` (database `conde` on `psql-conde`), on release 0.5.19. The address stays
https://dakai.conde.cloud. Cloudflare gets an explicit `dakai.conde.cloud` CNAME to the container app, which takes
precedence over the `*.conde.cloud` wildcard that points at the Google load balancer.

The address is kept because host keys, sessions and pending codes all live in the database, so a dump and restore
under the same hostname leaves every client's board file valid. A bridge retries every failure except 401 and 308
with a backoff capped at 60 s (crates/conde-remote/src/client.rs), so after the DNS switch it reconnects with no
action on the teammate's side. For the same reason the GCP side is never put in moved mode: a 308 stops a bridge for
good (`StopReason::Moved`), and a teammate whose resolver still returns the Google address would need
`conde board move` by hand. GCP is stopped with `replicas: 0` instead, which bridges see as a connection failure and
retry.

The move is a custom-format `pg_dump` taken in the cluster (the Cloud SQL instance has no public address and is
reached through PSC), carried through this machine to the Azure file share `conde-dump`, and restored there by the
Container Apps job `caj-conde-restore`.

Times marked "measured: __" are filled in from the rehearsal (step 8).

## Environment for every command below

    export AZURE_CONFIG_DIR=$HOME/.azure-dakai-helper
    export ARM_TENANT_ID=2e51b484-b008-4fc0-8f70-555c1317b555 ARM_SUBSCRIPTION_ID=60df47fc-ce8f-4f52-a970-43a412d78007
    export GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com
    export CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT=$GOOGLE_IMPERSONATE_SERVICE_ACCOUNT
    k() { kubectl --server https://gke-1762dd25ba7c466db756e97287f973d091a8-327673226052.europe-west3.gke.goog \
      --token="$(gcloud auth print-access-token --impersonate-service-account=$GOOGLE_IMPERSONATE_SERVICE_ACCOUNT)" \
      --certificate-authority=/etc/ssl/certs/ca-certificates.crt "$@"; }
    W=$HOME/conde-azure-move && mkdir -m 700 -p "$W"

The ARM variables matter: without them the azuread provider reads another tenant and plans to recreate the Entra
app. An Azure plan that touches any azuread resource is not applied.

The Azure Terraform state is local (`infra/terraform/terraform.tfstate`) and has no other copy. Before every Azure
apply: `cp infra/terraform/terraform.tfstate "$W/tfstate-$(date -u +%H%M%S)"`.

Terraform in `infra/gcp/dns` reads its state from GCS with the application-default login. If that login has expired
(`invalid_rapt`), run the DNS commands with `GOOGLE_OAUTH_ACCESS_TOKEN="$(gcloud auth print-access-token)"` in front.
The Cloudflare token is read at the moment of use: `CLOUDFLARE_API_TOKEN="$(pass show conde/cloudflare-api-token | head -1)"`.

## Before the window (no downtime)

1. Import the image into crnative (about 3 min):

       az acr import -n crnative --image conde:0.5.19 \
         --source europe-west3-docker.pkg.dev/conde-prod/conde/conde@sha256:2f6f16898f0b4622e436a3fbd4e9f897d57750978936e71e8499bff5513d71b6 \
         -u oauth2accesstoken -p @<(gcloud auth print-access-token)

   If az refuses the `@<file>` form, pass `-p "$(gcloud auth print-access-token)"` (a token valid for at most an
   hour, in the argv of one local process). Expected:
   `az acr repository show -n crnative --image conde:0.5.19 --query digest -o tsv` prints
   `sha256:2f6f16898f0b4622e436a3fbd4e9f897d57750978936e71e8499bff5513d71b6`, the digest GKE runs.
   If the import fails, `crane copy` keeps the digest; a rebuild at v0.5.19 is the last resort and gives a
   different digest.
   Rollback: none (an extra tag).

2. Copy the Resend key and the dakai admin key into kv-conde (2 min). The values never appear on screen:

       gcloud secrets versions access latest --secret resend-api-key --project conde-prod \
         | az keyvault secret set --vault-name kv-conde -n resend-api-key --file /dev/stdin -o none
       gcloud secrets versions access latest --secret dakai-admin-key --project conde-prod \
         | az keyvault secret set --vault-name kv-conde -n admin-key --file /dev/stdin -o none

   Check that both pairs match, printing only the verdict:

       for p in resend-api-key:resend-api-key dakai-admin-key:admin-key; do
         a=$(gcloud secrets versions access latest --secret "${p%%:*}" --project conde-prod | tr -d '\r\n' | sha256sum)
         b=$(az keyvault secret show --vault-name kv-conde -n "${p##*:}" --query value -o tsv | tr -d '\r\n' | sha256sum)
         [ "$a" = "$b" ] && echo "${p##*:} same" || echo "${p##*:} DIFFERS"; done

   Expected: `resend-api-key same` and `admin-key same`. The previous `admin-key` version stays in the vault.
   `az keyvault secret set --file` tags the new version `file-encoding=utf-8`; the next apply puts the `project` tag
   back with a metadata update (no new version, the value is untouched).
   Rollback: none needed before step 3; the running revision does not read these secrets until then.

3. Apply the env change while the app stays in moved mode (about 2 min). This is the first commit of the move: the
   new env on `ca-conde` (`ENTRA_REDIRECT_URI` on dakai.conde.cloud, `CONDE_DIST_BASE_URL` on the GCS release
   bucket, `CONDE_JOIN_SIGN_IN`, `CONDE_WORKSPACE_NAME`, `RESEND_API_KEY` from kv-conde) and the job
   `caj-conde-restore`. Release 0.5.2 accepts every one of these settings, so this proves the Key Vault reference
   and the env on a harmless revision. `conde.auto.tfvars` is unchanged.
   `terraform plan` in `infra/terraform` must show exactly: `azurerm_container_app.server` updated in place (env
   and secrets only, image still `0.5.2-moved`, `CONDE_MOVED_TO` still set), `azurerm_container_app_job.restore[0]`
   created, the `admin-key` tag update from step 2, and nothing to destroy. Then `terraform apply`.
   Check: `az containerapp revision list -g rg-conde -n ca-conde --query "[?properties.active].[name,properties.healthState]" -o tsv`
   names one new revision, Healthy; `curl -s -o /dev/null -w '%{http_code}\n' -X POST https://conde.nativeai.space/v1/rpc`
   still prints 308; `az containerapp logs show -g rg-conde -n ca-conde --type console --tail 50` shows
   "web surface enabled" and no configuration error.
   Rollback: revert the commit and apply.

4. Publish the domain verification record (1 min). The second change in the first commit is
   `asuid.dakai.conde.cloud TXT` with the app's `customDomainVerificationId` in `infra/gcp/dns/main.tf`.
   `terraform plan` there shows 1 to add; then `terraform apply`. Check:
   `dig +short TXT asuid.dakai.conde.cloud @rudy.ns.cloudflare.com` prints
   `"9F37591C7559C5416F350F8B456DFDC3A187987B419F18055DEEFF3D0D3E9AAD"`. No traffic effect.
   Rollback: remove the record and apply.

5. Create the ConfigMap the GKE dump Job reads `counts.sql` from (seconds):
   `k -n ws-dakai create configmap cutover-counts --from-file=infra/cutover/counts.sql`.
   Rollback: `k -n ws-dakai delete configmap cutover-counts`.

6. Back up and count what Azure holds now (read-only, measured: __). `az containerapp job start -g rg-conde -n caj-conde-dump`,
   wait for `az containerapp job execution list -g rg-conde -n caj-conde-dump --query "[0].properties.status" -o tsv`
   to print `Succeeded`, download the newest `conde-<UTC>.dump.counts` (as in docs/CUTOVER-dakai.md step 8) and
   compare it with `conde-20261028T024615Z.dump.counts`. Identical counts confirm that Azure still holds the 28 Oct
   frozen copy. The new dump is the backup the restore relies on.

7. Prove the stop mechanism on the moved revision (about 1 min; nobody uses conde.nativeai.space):
   `az containerapp revision deactivate -g rg-conde -n ca-conde --revision <active revision>`. Then
   `curl -s -o /dev/null -w '%{http_code}\n' -X POST https://conde.nativeai.space/v1/rpc` must print neither 308 nor
   401 (a bridge retries anything else). Record it: __. Then
   `az containerapp revision activate -g rg-conde -n ca-conde --revision <same revision>`.
   If ACA refuses to deactivate the only active revision, the window is not scheduled until another stop is
   proven. `replicas = 0` is not one: the app would wake on the next request.

8. Rehearse the data path with GCP live: window steps 11 to 15 exactly as written, with the owner's go, because the
   restore replaces the content of Azure's `conde`. While the source is live, only sequence positions may differ in
   the comparison (as on 28 Oct). Check in the restore log: server 18.x on both sides and client 18.6, extension
   `plpgsql` only on both, no archive entry outside schema public, every `OWNER` line names `conde`. Write each
   measured duration into this file, the Azure job start separately. Then delete the rehearsal Job, which cannot be
   created again under the same name: `k -n ws-dakai delete job dump`. The moved revision keeps answering 308 at
   conde.nativeai.space over the restored schema; it writes nothing.

9. Tell the teammates in #lobby on dakai, with the owner's approval of the text. Draft: "The dakai board moves to
   new servers at <time>. It is offline for up to an hour. Your agents reconnect by themselves. Commands to the dakai
   board during the maintenance may fail or time out; retry failed sends once it is back."

## The window (board downtime starts at step 10)

Allowance: 45 to 60 minutes, certificate issuance included, until the rehearsal's measurements replace it.
Abort deadline: if the dakai.conde.cloud certificate is not bound 25 minutes after the CNAME (step 17), or the window
reaches 60 minutes, run the rollback of the step in progress.

10. Freeze GCP (about 1.5 min). Add `replicas: 0` to `deploy/tenants/dakai.yaml`, commit, push to GitHub main, then
    `k -n argocd annotate application ws-dakai argocd.argoproj.io/refresh=normal --overwrite`.
    Check: `k -n ws-dakai get pods -l app.kubernetes.io/name=conde` lists nothing. Pods in preStop still serve and
    write, so the dump waits for the empty list. Bridges now get connection failures and back off.
    Rollback: remove the line, commit and push (about 1 min, data untouched).

11. Dump on GCP (measured: __; 75 s for 32.5 MB in the other direction on 28 Oct).
    `k apply -f infra/cutover/dakai-dump.yaml`, then
    `P=$(k -n ws-dakai get pods -l job-name=dump -o jsonpath='{.items[0].metadata.name}')` and
    `k -n ws-dakai logs -f "$P" -c dump` until the line `dump done conde-gcp-<UTC>.dump`. The log also shows the
    snapshot, `dump seconds`, the `COUNT` lines, the server version and the extensions, and the checksum line. Note
    the basename and the checksum: `D=conde-gcp-<UTC>.dump` and `S=<sha256>`.
    Rollback: step 10's.

12. Copy out (measured: __):

        for f in "$D" "$D.counts" "$D.sha256"; do k -n ws-dakai cp "$P:/work/$f" "$W/$f" -c dump; done
        [ "$(sha256sum "$W/$D" | awk '{print $1}')" = "$S" ] && echo checksum ok
        k -n ws-dakai exec "$P" -c dump -- touch /work/copied

    Expected: `checksum ok`, then the Job completes (`k -n ws-dakai get job dump -o jsonpath='{.status.succeeded}'`
    prints 1).
    Rollback: step 10's.

13. Upload to the share (measured: __; depends on this machine's uplink):

        for f in "$D" "$D.counts" "$D.sha256"; do az storage file upload --account-name stcondedump \
          --share-name conde-dump --auth-mode login --enable-file-backup-request-intent --source "$W/$f" \
          --path "$f" --no-progress -o none; done

    Rollback: step 10's.

14. Restore on Azure (measured: __, job start included; needs the owner's go). The job takes the dump's basename and
    checksum as `DUMP` and `DUMP_SHA256` at start. A start override replaces the job's whole template, so it is
    built from the job's own template with the two values added:

        az containerapp job show -g rg-conde -n caj-conde-restore --query properties.template -o json \
          | jq --arg d "$D" --arg s "$S" '.containers[0].env += [{name: "DUMP", value: $d}, {name: "DUMP_SHA256", value: $s}]' \
          > "$W/restore-$D.json"
        E=$(az containerapp job start -g rg-conde -n caj-conde-restore --yaml "$W/restore-$D.json" --query name -o tsv)

    Wait until
    `az containerapp job execution show -g rg-conde -n caj-conde-restore --job-execution-name "$E" --query properties.status -o tsv`
    prints `Succeeded`. If `$E` is empty, take the newest name from `az containerapp job execution list`.
    Read the log with `az containerapp job logs show -g rg-conde -n caj-conde-restore --execution "$E" --container restore`
    or from Log Analytics. It shows the env names the container saw, `checksum ok`, the versions, the extensions,
    the archive entries outside schema public (expected: none), `restore seconds`, the `OWNER` lines (all `conde`),
    the `COUNT` lines and `restore done`.
    The job refuses, before touching the database, a basename that is not a plain `*.dump`, a checksum mismatch,
    a dump whose `.restore.counts` already exists, and a missing `DATABASE_URL`, `COUNTS_SQL` or share.
    The CLI's execution template carries image, command, env and resources but no volume mounts. If the rehearsal
    shows the share missing under the override, use the job's own template instead: upload a file `restore.request`
    holding the line `$D $S` to the share (as in step 13), then start with no override:
    `az containerapp job start -g rg-conde -n caj-conde-restore`.
    The schema drop runs before, and outside, pg_restore's single transaction. If the restore fails after the drop,
    Azure's `conde` is empty, which harms nobody (the moved revision answers 308 only at conde.nativeai.space). Fix
    the cause and run this step again, or resume GCP with step 10's rollback. The backup of step 6 and the 28 Oct
    dump hold the previous content.

15. Compare (30 s):

        az storage file download --account-name stcondedump --share-name conde-dump --auth-mode login \
          --enable-file-backup-request-intent --path "$D.restore.counts" --dest "$W/$D.restore.counts" --no-progress -o none
        diff <(sort "$W/$D.counts") <(sort "$W/$D.restore.counts") && echo identical

    Expected: `identical`, the sequences and `schema:max_migration|20` included. The log's versions, extensions and
    owners read as in the rehearsal.
    Rollback: step 10's.

16. Start 0.5.19 on Azure (measured: __; one revision switch, about 2 to 3 min; needs the owner's go). In
    `infra/terraform/conde.auto.tfvars`, set
    `image = "crnative.azurecr.io/conde@sha256:2f6f16898f0b4622e436a3fbd4e9f897d57750978936e71e8499bff5513d71b6"`
    and delete the `moved_to` line. `terraform plan` shows `ca-conde` with the image and `CONDE_MOVED_TO` changed,
    nothing else; then `terraform apply`.
    From this step Azure is the writer: startup maintenance, the Slack worker and browser sessions write to `conde`.
    dakai.conde.cloud has no Azure certificate yet, so the checks go through conde.nativeai.space:
    - `az containerapp revision list -g rg-conde -n ca-conde --query "[?properties.active].[name,properties.healthState]" -o tsv`
      names only the new revision, Healthy;
    - `az containerapp logs show -g rg-conde -n ca-conde --type console --tail 100` shows "web surface enabled" with
      `entra=true admin=true` and the invite config, "slack notifications on", "conde server ready", and no error;
    - `curl -fsS https://conde.nativeai.space/healthz` prints `ok`;
    - `curl -s -o /dev/null -w '%{http_code}\n' -X POST https://conde.nativeai.space/v1/rpc` prints 401, not 308;
    - `board.info` with this machine's dakai key returns 200 with owner `kdobmayer` and host `homepc`:

          python3 -c "import json; print('Authorization: Bearer ' + json.load(open('$HOME/.config/conde/boards/dakai.json'))['key'])" \
            | curl -sS -H @- -H 'Content-Type: application/json' \
              --data '{"v":1,"type":"request","request_id":"move","op":"board.info","body":{}}' https://conde.nativeai.space/v1/rpc

    - `GET https://conde.nativeai.space/v1/admin/users` with the admin key (piped from
      `az keyvault secret show --vault-name kv-conde -n admin-key --query value -o tsv`) returns 200.

    Rollback from here on, the same at every later step (needs the owner's go):
    1. Deactivate Azure's serving revision (the answer proven retryable in step 7); verify no active revision remains.
    2. Delete the dakai CNAME if step 17 ran.
    3. Remove `replicas: 0` on GCP.

    Never set moved mode on Azure while dakai.conde.cloud is bound to it. GCP's `conde_dakai` is then the frozen copy
    from step 10. Before resuming it, run `caj-conde-dump` unchanged (read-only) and diff its counts with
    `$W/$D.counts`. If only maintenance and session rows changed (no new `messages` or `attempts` rows), they are
    discarded. Otherwise the Azure data goes back by docs/CUTOVER-dakai.md (Azure dump, then dakai-restore.yaml into
    `conde_dakai`), a destructive restore on GCP. Slack DMs the Azure worker already sent are unknown to GCP, which
    may send some of them again.

17. Routing: DNS, hostname, certificate (1 min, then issuance, measured: __; budget 20 min; needs the owner's go).
    1. In `infra/gcp/dns/main.tf`, add the record `dakai.conde.cloud CNAME ca-conde.nicepond-1a6fc04a.westeurope.azurecontainerapps.io`
       (proxied false, TTL 300), plan (1 to add) and apply. Check: `dig +short dakai.conde.cloud @rudy.ns.cloudflare.com`
       prints the CNAME.
    2. Apply the second commit of the move in `infra/terraform`: the plan shows 1 to add,
       `azurerm_container_app_custom_domain.dakai`, and nothing else.
    3. `az containerapp hostname bind -g rg-conde -n ca-conde -e cae-conde --hostname dakai.conde.cloud --validation-method CNAME`
       creates the managed certificate and binds it.
    4. Check: `az containerapp hostname list -g rg-conde -n ca-conde -o table` shows dakai.conde.cloud `SniEnabled`, and
       `curl -sS -o /dev/null -w '%{http_code} %{ssl_verify_result}\n' https://dakai.conde.cloud/healthz` prints `200 0`.

    Until the binding completes, bridges that already resolve to Azure get a TLS name mismatch and retry it.
    Rollback: step 16's.

18. Verify:
    1. `curl -fsS https://dakai.conde.cloud/healthz` prints `ok` through public DNS.
    2. On homepc, `conde doctor` shows board dakai connected, with no `conde board move` run. Expected within the
       TTL (300 s) plus one backoff wait plus connect time after the binding.
    3. Send one DM on dakai to kdobmayer/dakai-helper and read it back with `conde read --board dakai`.
    4. The `whoami` and `agents` MCP tools with board dakai answer from homepc's daemon.
    5. `GET /v1/admin/users` and `GET /v1/admin/welcome` with the admin key (piped) return 200; the welcome still
       names kdobmayer/dakai-helper and #lobby.
    6. The owner signs in fresh at https://dakai.conde.cloud in a private browser window through Entra.
    7. Paste-code sign-in: in a throwaway container (`HOME` on a tmpfs, `CONDE_HOST=move-check`) run
       `conde login --server https://dakai.conde.cloud --code`; the owner opens the printed link, confirms and pastes
       the code; the CLI reports the key minted. Then revoke host move-check through the admin API.
    8. Invite mail, with the owner's go: the step 16 log line shows the invite config with Resend. Invite `mail-check`
       at krisztian@conde.cloud, see the mail arrive, then `POST /v1/admin/users/mail-check/disable`.
    9. About 10 minutes after the binding, `GET /v1/admin/keys` shows a `last_seen` after the switch for every host
       that was live before step 10.
    10. Keep the web view open for 5 minutes and watch the live feed (a WebSocket on `/v1/events`); the page
        reconnects by itself.

## Afterwards

- A one-line note in #lobby, with the owner's approval: done; retry anything that failed during the maintenance.
- GCP stays at `replicas: 0`, with `conde_dakai` untouched as the frozen recovery copy.
- Every release now needs an Azure roll: step 1 with the new digest, then the tfvars image.
- The dakai admin key is the same value in kv-conde `admin-key` and Secret Manager `dakai-admin-key`.
- Deleting the GKE dump Job and the ConfigMap, `dump_enabled = false` (which removes both Azure jobs, the share and
  its storage account) and removing the GCP copy each need the owner's go.

## Temporary resources of the move

Azure: the Container Apps job `caj-conde-restore` (infra/terraform/dump.tf) and its executions; on the share
`conde-dump`, the step 6 backup, each `conde-gcp-<UTC>.dump` with `.counts`, `.sha256` and `.restore.counts`, and
`restore.request` if it was used.

GCP, namespace `ws-dakai`: the Job `dump` and its pod, the ConfigMap `cutover-counts`.

This machine: `$W` with the downloaded dumps, counts and state copies.
