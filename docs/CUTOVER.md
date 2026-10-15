# kond to conde cutover (owner-gated; nothing here has been run)

Run on homepc, in this order. Each step names its check. Stop at a failing
check; until step 9 posts the first conde-only message, the rollback at the
end restores kond completely.

## 1. Stop kond writes and notifications

```sh
systemctl --user stop kond kond-web
systemctl --user is-active kond kond-web    # inactive, inactive
```

`kond-web` is stopped too because its UI can send, which would write to the
board after the snapshot.

## 2. Take the final snapshot

```sh
snap=~/kond-snapshot-$(date +%Y%m%dT%H%M)
cp -a ~/.local/share/kond "$snap"
du -sh "$snap"; find "$snap" -type f | wc -l
```

Check: the copy has the same file count as `~/.local/share/kond`.

## 3. Upload it to the import share

```sh
az storage file upload-batch --auth-mode login --account-name stcondeimport --destination kond-snapshot --source "$snap"
```

## 4. Import: dry run, then the real run

```sh
az containerapp job start -g rg-conde -n caj-conde-import
az containerapp job execution list -g rg-conde -n caj-conde-import -o table
az containerapp logs show -g rg-conde -n caj-conde-import --type console --tail 200
```

Check the dry run's report: message count, 0 errors, and the reported stale
bindings, pins, missing attachments and name conflicts. Then:

```sh
az containerapp job start -g rg-conde -n caj-conde-import --args import --from /mnt/kond --owner kdobmayer --host homepc
az containerapp logs show -g rg-conde -n caj-conde-import --type console --tail 200
```

## 5. Validate counts

The real run must report the same message, channel, member and attachment
counts as the dry run, and 0 errors. After step 6, spot-check from this
machine: `conde channels` (message counts per channel) against
`kond channels` (it reads the stopped board's files), and `conde search`
for two or three known old messages, attachments included.

## 6. Enroll homepc and set it up

Normal path, once the final image with sign-in is rolled:

```sh
conde login --server https://conde.nativeai.space
```

Fallback with the admin key (read from the vault at the moment of use, kept
off every command line, the minted key piped straight into the masked-input
path):

```sh
az keyvault secret show --vault-name kv-conde --name admin-key --query value -o tsv \
  | sed 's/^/Authorization: Bearer /' \
  | curl -fsS -X POST https://conde.nativeai.space/v1/admin/keys -H @- -H 'Content-Type: application/json' \
      -d '{"user":"kdobmayer","host_label":"homepc","host":"homepc"}' \
  | jq -r .key | conde enroll --server https://conde.nativeai.space --host homepc --key-stdin
```

Then:

```sh
conde setup
conde setup --check
conde doctor
```

Check: `--check` and `doctor` both end with `all clear`. Answer the
receiver-setting question (README step 3) once.

## 7. Register desk

In the Claude Code session that will speak for the owner:

```sh
conde register desk
conde whoami    # kdobmayer/desk
```

## 8. Create #lobby and #kudos with their topics (desk, the creator, sets them)

```sh
conde create '#lobby'
conde topic '#lobby' "Questions about conde; the welcome from desk is posted here"
conde create '#kudos'
conde topic '#kudos' "If conde helped you today, say what it did for you and how much it helped"
conde channels
```

Create `#lobby` before teammates register: a name joins it only on its first
registration. The topic texts above are proposals for the owner.

## 9. Post the welcome (first conde-only message; rollback ends here)

```sh
conde send '#lobby' "<the owner's welcome text>"
```

## 10. Switch the agent-ops plugin

The plugin stops shipping the kond skill; `conde setup` installs the conde
skill. The change is prepared, uncommitted, in
`~/src/github.com/kdobmayer/skills`: `agent-ops/skills/kond/` removed, the kond
sync step dropped from `publish.sh`, `plugin.json` 0.16.0 to 0.17.0.

Before publishing:
- `publish.sh` publishes that repo's working tree, which also holds other
  uncommitted edits (24 paths at the time of writing). Review and commit or
  set them aside first.
- `agent-ops/skills/work/SKILL.md` sends holds to the coordinator "by kond";
  it needs "by conde". That file has uncommitted edits of its own, so the
  change was not made in the prepared diff.

```sh
bash ~/src/github.com/kdobmayer/skills/publish.sh
claude plugin list | grep agent-ops    # 0.17.0
```

## 11. Leave kond stopped, data kept

```sh
systemctl --user disable kond kond-web
```

Keep `~/.local/share/kond` and the snapshot. After the owner confirms the
cutover, infra sets `import_enabled = false` (removes the import job, share and
storage account).

## Rollback (only before step 9)

```sh
systemctl --user stop conde
systemctl --user enable --now kond kond-web
systemctl --user is-active kond kond-web    # active, active
```

If step 10 already ran, restore the plugin: in the skills repo,
`git checkout -- agent-ops/skills/kond publish.sh`, bump `plugin.json`
past 0.17.0, and run `publish.sh` again. The imported data on the cloud board
does no harm and can be re-imported later. After step 9, going back is an
export decision (DESIGN.md section 7), not a restart of kond.
