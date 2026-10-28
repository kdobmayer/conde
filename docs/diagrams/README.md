# conde.cloud estate drawing

`conde-cloud-estate.svg` (with `.png` at 2x and `.pdf`) is one picture of the GCP estate as the code describes it:
the trust boundaries (internet, Cloud Armor, IAP on the admin hosts, the private VPC, Private Service Connect to
Cloud SQL, one namespace per workspace) and the flows between the people, the cluster and the services it calls.
The old Azure estate is not drawn.

Read from master at `45a8f57` ("Backlog: S8 housekeeping from the diagram read"), which holds the merged `s4-iap`
(admin surfaces behind IAP, the IP allowlist retired) and the cost cuts (demo and clearscale at one replica, dakai at
two, Keycloak at 250m and 768Mi): `infra/gcp` (foundation, platform, cluster, workspaces, modules/workspace, realms,
dns), `charts/conde-workspace`, `deploy/`, `.github/workflows`, `docs/COST-conde-cloud.md`, `docs/infra-plan-gcp.md`,
`docs/infra-gcp-s4-iap.md`, and `crates/conde-remote/src/web/auth.rs` for how conde reaches its OIDC issuer. It
shows the code at that commit. Whether every part is applied was not checked against the live project.

Rebuild with `./build.sh` (Python 3 with Pillow, rsvg-convert, the Inter and IBM Plex Mono fonts). `build_estate.py`
draws it. The layout comparison is in `REVIEW.md`; the other option's script is in the history at `29710b0`.
`diagramkit.py` and `icons/` are pinned copies, so the drawing rebuilds without the original kit.
`COMPONENTS.md` lists every drawn element with its source line.
