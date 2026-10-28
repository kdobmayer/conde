# Review of the estate drawing

Stance: a demanding design lead asking whether the owner would notice a fault within a minute. Every render was
inspected at 2x in 3 x 2 tiles (`crop.py`) and as the 1/12 thumbnail. Component set in `COMPONENTS.md`. Source read
at master `45a8f57`.

## Layout

| Option | Layout | Score |
|---|---|---|
| A (round 1, `29710b0`) | zones as columns: people and Cloudflare left, edge column (load balancer, admin and public policies), cluster with the namespaces as rows, managed services under the VPC, internet services and GitHub right | 7.0 |
| B `build_estate.py` | zones as stripes: people and Cloudflare in a top band, then the load balancer bar, the public and admin policies, the cluster with the namespaces as columns (workspaces, Keycloak, Argo CD), managed services at the bottom, internet services and GitHub right | 8.5 |

Pick B. Its stripes are the trust boundaries in the order a request crosses them (internet, load balancer, Cloud
Armor and IAP, VPC, namespace, Private Service Connect, Cloud SQL), and every request edge is one straight vertical.
A carried the same facts on a shorter canvas but left loose gaps in both side columns, and its policy cards were
taller than their content because they had to span the rows they fed. The redraw at `45a8f57` changes content only,
so the comparison stands.

## Round 2 corrections (coordinator review)

- conde's OIDC for demo and clearscale is drawn leaving the workspaces for auth.conde.cloud and coming back through
  the Gateway and conde-web to Service keycloak; the issuer URLs and discovery in `auth.rs` decide it. Only demo's
  admin API stays in the cluster, drawn as an in-cluster call (its own legend entry), "demo only, internal HTTP".
  Argo CD's sign-in through realm infra via the Gateway is a note.
- Cloud NAT is "public internet egress". The IAP sign-in moved from the Google node to IAP and the owner's browser;
  the Google node keeps Keycloak's broker back-channel.
- A note names the Kubernetes Secrets that hold values: `iap-admin` in keycloak and argocd (Terraform, write-only from
  Secret Manager) and Argo CD's own credentials and repository key. Demo's CSI files include the Keycloak admin
  client secret.
- Replicas after the cost cuts: dakai 2, demo 1, clearscale 1.

## Drawn differently from the brief

Argo CD's rollout of the workspaces is named in its notes (controller syncs ws-*, ApplicationSet) instead of a line,
which would have crossed Keycloak. The egress is one edge from the cluster to Cloud NAT, and each internet service
names its caller in its note. Microsoft Entra ID has no product icon in the Azure pack V24, so the Dakai tenant uses
the official App Registrations icon; Slack has no icon in Simple Icons and gets the kit's neutral cloud glyph.

## Not verified

That the estate is applied as this commit describes it: no read of the live project or cluster.

## Checks

Tiles 3 x 2 clean, silhouette shows the top band, the edge stripe, the VPC with the namespace columns, the managed
services and the right column. Label grep clean: every required name present, no Azure, nativeai, stconde, date,
em or en dash or semicolon. `plate_check.py` on the A4 landscape default fails (median label 3.9 pt); the drawing is
a screen and large-format picture for review, not an A4 plate (the PDF page is 2175 x 1482 pt, labels at 12 pt).
