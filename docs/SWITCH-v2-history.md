# History restore at the homepc switch (H2 + H3)

Run on homepc by the owner, with the v2 `conde` binary, before the first v2 daemon start. It
builds the new local board from a fresh snapshot of the kond board (H2) and adds the owner's
conde-native DMs that the purge of the shared board saved (H3). Nothing on the shared board
changes. On any failure, delete the local board and rerun from step 4; a partial board never
starts.

```sh
# 1. Stop every writer: kond carries private traffic until now; conde is the v1 daemon.
systemctl --user stop conde kond kond-web
systemctl --user is-active conde kond kond-web            # inactive, inactive, inactive

# 2. A fresh snapshot of the kond board, checked by its file count.
snap=~/.cache/conde-snapshot-switch
rm -rf "$snap" && mkdir -p "$snap" && cp -a ~/.local/share/kond/. "$snap"/
n=$(find "$snap" -type f | wc -l)
test "$n" -eq "$(find ~/.local/share/kond -type f | wc -l)" && echo "snapshot: $n files"

# 3. The prune list: every kond name, oldest last activity first. Write the names to drop,
#    one per line, to the prune file (left empty, nothing is dropped).
conde history prune-candidates --snapshot "$snap" --out ~/.cache/conde-prune-candidates.tsv
touch ~/.cache/conde-prune.txt

# 4. H2 into the new, empty local board. It must end "result: OK": every source row present,
#    every body and attachment sha256-equal to the snapshot, the pruned rows counted as skipped.
test ! -e ~/.local/share/conde/conde.db
env -u DATABASE_URL conde import --from "$snap" --expect-files "$n" --owner kdobmayer --host homepc \
  --skip-agents ~/.cache/conde-prune.txt

# 5. H3 from the purge's saved rows, dry run first. It must end "result: OK": the frozen class (b)
#    rows equal to the manifest's hashes, the newer DMs equal to the saved rows, every reply-to,
#    read state and reaction equal (49 rows when nothing is pruned: 32 frozen, 17 newer).
csv=~/.cache/conde-central-purge-2026-10-09/central-purge-2026-10-09
man=~/work/ai/conde/docs/history-manifest-2026-10-09
conde history copy --manifest "$man" --from-csv "$csv" --after-freeze --skip-agents ~/.cache/conde-prune.txt --dry-run
conde history copy --manifest "$man" --from-csv "$csv" --after-freeze --skip-agents ~/.cache/conde-prune.txt

# On a failure in step 4 or 5:  rm -f ~/.local/share/conde/conde.db*  and rerun from step 4.
```

What each part covers:

- The snapshot holds the kond history, including the private traffic kond carried after its
  restart (2026-10-09 09:25 UTC). The vvp-alpha outbox files set aside under
  `~/.cache/kond-outbox-held-2026-10-09` are not in it.
- The prune file drops a name with every message it sent or received, its channel memberships and
  its reactions, from both steps. A name the snapshot does not know stops the import.
- The manifest (`docs/history-manifest-2026-10-09*`, local only) is the H1 freeze at commit_seq
  18724. Its class (b) has 41 rows: 32 were saved by the purge (desk with astra) and are copied;
  9 (desk with dakai-helper) stay on the shared board, where both names remain.
- `--after-freeze` adds the 17 DMs the purge saved that are newer than the freeze (desk, astra,
  vvp-alpha, vvp-astra, native-fable and native-astra with each other); without it they are not
  restored anywhere. They are checked against the saved rows, since no manifest lists them.
- A copied DM older than a kond DM between the same two names is listed after it on the local
  board (the copy reports each one); its read state is its own, so nothing else changes.
