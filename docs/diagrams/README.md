# conde.cloud estate drawing

`conde-cloud-estate.svg` (with `.png` at 2x and `.pdf`) is one picture of the GCP estate as the code describes it:
the trust boundaries (internet, Cloud Armor, IAP on the admin hosts, the private VPC, Private Service Connect to
Cloud SQL, one namespace per workspace) and the flows between the people, the cluster and the services it calls.
The old Azure estate is not drawn.

Read from the branch `s4-iap` at commit `3458a3e` ("Retire the admin IP allowlist"), the successor of `dae0952` and
`808ec7b`: `infra/gcp` (foundation, platform, cluster, workspaces, modules/workspace, realms, dns),
`charts/conde-workspace`, `deploy/`, `.github/workflows`, `docs/COST-conde-cloud.md`, `docs/infra-plan-gcp.md`,
`docs/infra-gcp-s4-iap.md`. It shows the code at that commit. Whether every part is applied was not checked
against the live project.

Rebuild with `./build.sh` (Python 3 with Pillow, rsvg-convert, the Inter and IBM Plex Mono fonts). `build_estate.py`
draws the picked layout. `build_estate_option_a.py` draws the other option, kept for the comparison in `REVIEW.md`.
`diagramkit.py` and `icons/` are pinned copies, so the drawing rebuilds without the original kit.
`COMPONENTS.md` lists every drawn element with its source line.
