# Dakai cutover: conde.nativeai.space to dakai.conde.cloud

The Dakai board moves from the Azure Container App `ca-conde` (database `conde` on `psql-conde`) to the GKE workspace
`ws-dakai` (database `conde_dakai` on Cloud SQL `conde-pg`, owned by the IAM user `conde-dakai@conde-prod.iam`). The
move is a custom-format `pg_dump` taken on Azure, carried through this machine and restored in the cluster. The steps
below were rehearsed on 2026-10-28 between 01:31 and 01:58 UTC, and each duration given is from that run.

## What the rehearsal showed

| step | measured |
|---|---|
| dump job (container start, pg_dump, counts in the same snapshot) | 75 s for a 32.5 MB dump (23 tables, 783,035 rows in `attempts`) |
| download from the share | 5 s |
| restore pod start (proxy sidecar ready) | 3 s |
| upload into the pod (`kubectl cp`) | 11 to 12 s |
| restore (schema reset, `pg_restore --single-transaction`, dbinit grants) | 16 s; 21 to 26 s with the owner and count checks |
| server stop (`replicas` 0 until the pods are gone) | 6 s |
| server start (Argo CD applies `replicas` 2 until both pods are ready / `/healthz` answers through the load balancer) | 7 s / 9 s |

During the freeze the machines need just over two minutes: dump 75 s, download 5 s, restore pod 3 s, upload 12 s,
restore 26 s, start 9 s. On top of that come the old app's revision switch (about a minute per docs/DEPLOY.md; not
measured here, since the rehearsal did not touch the old side), one push to GitHub main with an Argo CD refresh, and
the time to type the commands. A planned window of 15 minutes leaves plenty of margin.

The rehearsal stopped and started the server by pausing Argo CD's automation with a targeted apply of
`workspaces_auto_sync` in `infra/gcp/cluster` (about 15 s each way) and scaling by hand, because no push to GitHub was
available mid-run. The cutover uses the tenant file's `replicas` instead, so git stays the only path.

Rehearsal results:

- Every row count, `max(commit_seq)` (19590) and `max(message_id)` of `messages`, and the schema version (16) in
  `conde_dakai` after the restore equal the dump's snapshot. The only difference was the position of
  `attempts_attempt_seq_seq`: sequences are outside the snapshot, and the live server advanced it between pg_dump
  reading it and the count query. After the restore it equals `max(attempt_seq)` (783084), so new rows cannot
  collide. Once the old server is frozen, nothing moves and the comparison is exact.
- All 23 tables, 51 indexes and 4 sequences, the `public` schema and the database are owned by
  `conde-dakai@conde-prod.iam`.
- The server applied migration 17 on its first start. The 8 identities now carry the issuer
  `https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0`, and `identities_one_per_issuer`
  exists. The server logged no errors.
- `board.info` with this machine's dakai host key returned 200 with owner `kdobmayer` and host `homepc`. A wrong key
  gets 401.
- Not exercised: browser sign-in through Entra.

The rehearsal data is live on dakai.conde.cloud as of the 01:31 UTC snapshot. Nobody should point a host at it before
the cutover, because the cutover wipes it.

## Before the window

Environment for every command below:

    export AZURE_CONFIG_DIR=$HOME/.azure-dakai-helper
    export GOOGLE_IMPERSONATE_SERVICE_ACCOUNT=terraform@conde-prod.iam.gserviceaccount.com
    export CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT=$GOOGLE_IMPERSONATE_SERVICE_ACCOUNT
    k() { kubectl --server https://gke-1762dd25ba7c466db756e97287f973d091a8-327673226052.europe-west3.gke.goog \
      --token="$(gcloud auth print-access-token --impersonate-service-account=$GOOGLE_IMPERSONATE_SERVICE_ACCOUNT)" \
      --certificate-authority=/etc/ssl/certs/ca-certificates.crt "$@"; }
    W=$HOME/conde-cutover && mkdir -m 700 -p "$W"

1. Build the old app's next image. The image it runs now (`conde:8296717`) predates `CONDE_MOVED_TO`. The variable
   and migration 0017 arrived together in 0c4ffff, so freezing the old server requires a newer image. Build the
   release the GCP side runs (0.2.0, master's tree):
   `az acr build --subscription 60df47fc-ce8f-4f52-a970-43a412d78007 -r crnative --platform linux/amd64 -t conde:0.2.0 -f Dockerfile .`
2. Make sure every teammate's client is 0.2.0 or later (`conde --version`; `conde update`), because `conde board move`
   ships in 0.2.0.
3. Delete the rehearsal Jobs (their pod template is immutable, so the restore Job cannot be created again under the
   same name): `k -n ws-dakai delete job restore restore-2 verify`.
4. Stop the new server through git. Add `replicas: 0` to `deploy/tenants/dakai.yaml`, commit, and push to GitHub
   main. Then refresh Argo CD instead of waiting out its three-minute poll:
   `k -n argocd annotate application ws-dakai argocd.argoproj.io/refresh=normal --overwrite`. The new server serves
   nobody yet, so this step can be done any time before the window. Check:
   `k -n ws-dakai get pods -l app.kubernetes.io/name=conde` lists nothing.

## The window

5. Freeze the old server. In `infra/terraform/conde.auto.tfvars`, set `image = "crnative.azurecr.io/conde:0.2.0"` and
   add `moved_to = "https://dakai.conde.cloud"`, then run `terraform apply` from `infra/terraform` (one revision
   switch, about a minute). The new image applies migration 0017 to the Azure database as it starts. This is the only
   write to the old database, and from then on only images of 0c4ffff or later run against it (a rollback is
   `moved_to = ""` with image 0.2.0, never 8296717 again). In moved mode the server writes nothing else, and its
   Slack worker is off.
6. Check the freeze. `curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' -X POST https://conde.nativeai.space/v1/rpc`
   prints 308 and https://dakai.conde.cloud (the Location of every answer but `/healthz`), and
   `az containerapp revision list -g rg-conde -n ca-conde --query "[?properties.active].name" -o tsv` names only the
   new revision. Start the dump only after both hold, so that no write from the previous revision lands after it.
7. Dump (75 s): `az containerapp job start -g rg-conde -n caj-conde-dump`. Then wait until
   `az containerapp job execution list -g rg-conde -n caj-conde-dump --query "[0].properties.status" -o tsv` says
   `Succeeded`.
8. Download (5 s):

       D=$(az storage file list --account-name stcondedump --share-name conde-dump --auth-mode login \
         --enable-file-backup-request-intent --query "sort_by([?ends_with(name, '.dump')], &name)[-1].name" -o tsv)
       for f in "$D" "$D.counts" "$D.sha256"; do az storage file download --account-name stcondedump \
         --share-name conde-dump --auth-mode login --enable-file-backup-request-intent --path "$f" --dest "$W/$f" \
         --no-progress -o none; done
       [ "$(awk '{print $1}' "$W/$D.sha256")" = "$(sha256sum "$W/$D" | awk '{print $1}')" ] && echo checksum ok

9. Create the restore Job (3 s until it waits for the dump):
   `k apply -f infra/cutover/dakai-restore.yaml`, then
   `P=$(k -n ws-dakai get pods -l job-name=restore -o jsonpath='{.items[0].metadata.name}')` once the pod is
   `Running`.
10. Upload (12 s):
    `k -n ws-dakai cp "$W/$D" "$P:/work/conde.dump" -c restore && k -n ws-dakai cp infra/cutover/counts.sql "$P:/work/counts.sql" -c restore`
11. Restore (16 s; the rehearsal data goes with the schema reset):
    `k -n ws-dakai exec "$P" -c restore -- touch /work/ready`. Wait for
    `k -n ws-dakai get job restore -o jsonpath='{.status.succeeded}'` to print 1, then save the log with
    `k -n ws-dakai logs "$P" -c restore > "$W/restore.log"`. The log shows `restore seconds`, every `OWNER` line names
    `conde-dakai@conde-prod.iam`, and no line starts with `ERROR`.
12. Compare: `diff <(sort "$W/$D.counts") <(grep '^COUNT ' "$W/restore.log" | sed 's/^COUNT //' | sort) && echo identical`.
    With the old server frozen, every line must match, the sequences and `schema:max_migration|17` included.
13. Start the new server (9 s once Argo CD applies it): remove `replicas: 0` from `deploy/tenants/dakai.yaml`, commit,
    push to GitHub main, then
    `k -n argocd annotate application ws-dakai argocd.argoproj.io/refresh=normal --overwrite`. Wait for
    `k -n ws-dakai get deploy conde -o jsonpath='{.status.readyReplicas}'` to print 2.
14. Check the server:
    - `curl -fsS https://dakai.conde.cloud/healthz` prints `ok`.
    - `k -n ws-dakai logs deploy/conde -c conde | grep -iE 'error|panic'` prints nothing. Migration 17 is already in
      the dump, so no `applied migration` line appears this time.
    - A host key from `~/.config/conde/boards/dakai.json`, sent without printing it, gets a 200 with owner
      `kdobmayer`:

          python3 -c "import json; print('Authorization: Bearer ' + json.load(open('$HOME/.config/conde/boards/dakai.json'))['key'])" \
            | curl -sS -H @- -H 'Content-Type: application/json' \
              --data '{"v":1,"type":"request","request_id":"cutover","op":"board.info","body":{}}' https://dakai.conde.cloud/v1/rpc

15. Sign in at https://dakai.conde.cloud in a browser with an Entra account.
16. Every teammate runs `conde board move dakai https://dakai.conde.cloud`. It checks the old server's 308 from step 5
    before it sends the key anywhere.

## Afterwards

- Remove the dump path: set `dump_enabled = false` in `infra/terraform/conde.auto.tfvars` and apply (it removes the
  five dump resources listed below), then delete the local copies (`rm -r "$W"`).
- Delete the restore Job: `k -n ws-dakai delete job restore`.
- The old Container App stays in moved mode for a week, then is removed on the owner's go.

## Temporary resources created by the rehearsal

Azure (the first four entries are the five Terraform resources of `infra/terraform/dump.tf`, share included, which
`dump_enabled = false` removes):

- storage account `stcondedump` (rg-conde), holding the file share `conde-dump` with `conde-20261028T013124Z.dump`,
  `.dump.counts` and `.dump.sha256`
- Container Apps environment storage `conde-dump` on `cae-conde` (ReadWrite)
- Container Apps Job `caj-conde-dump` (execution `caj-conde-dump-crti0db`)
- role assignment `f99c846e-7ef1-4340-c079-2ad35462dbae`: Storage File Data Privileged Contributor on `stcondedump`
  for krisztian.dobmayer@dakai.io
- executions `caj-conde-psql-fny1mvk` and `caj-conde-psql-veh31kz` of the existing `caj-conde-psql` job (count
  queries; execution history only)

GCP, namespace `ws-dakai`:

- Job `restore` (failed on a typo in its owner query after the restore itself had completed), Job `restore-2` (the
  restore that counts), Job `verify` (a read-only check after the start), and their pods
