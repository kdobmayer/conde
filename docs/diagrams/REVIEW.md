# Review of the estate drawing

Stance: a demanding design lead asking whether the owner would notice a fault within a minute. Every render was
inspected at 2x in 3 x 2 tiles (`crop.py`) and as the 1/12 thumbnail. Component set in `COMPONENTS.md`. Source read
at `s4-iap` `3458a3e`.

## conde.cloud estate

| Option | Layout | Score |
|---|---|---|
| A `build_estate_option_a.py` | zones as columns: people and Cloudflare left, edge column (load balancer, admin and public policies), cluster with the namespaces as rows (Argo CD, Keycloak, workspaces), managed services under the VPC, internet services and GitHub right | 7.0 |
| B `build_estate.py` | zones as stripes: people and Cloudflare in a top band, then the load balancer bar, the public and admin policies, the cluster with the namespaces as columns (workspaces, Keycloak, Argo CD), managed services at the bottom, internet services and GitHub right | 8.5 |

Pick B. Its stripes are the trust boundaries in the order a request crosses them (internet, load balancer, Cloud
Armor and IAP, VPC, namespace, Private Service Connect, Cloud SQL), and every request edge is one straight vertical.
The three workspaces stand side by side as peers of Keycloak and Argo CD. A gets the same facts on a shorter canvas
but leaves loose gaps in both side columns, and its policy cards and load balancer box are taller than their content
because they have to span the namespace rows they feed.

Fixed on the way: Terraform's edge crossed the project title and a card title (moved right of both, the control
plane card dropped), an edge crossed the Keycloak card title (title shortened), the Cloudflare items touched each
other and the card border, the Secret Manager and tfstate nodes overran the Data section, the image pull edge
crossed the Delivery kicker, the promote label sat on the Actions label, the DNS endpoint note gained the
port-forward to Keycloak, which the label grep found missing.

Drawn differently from the brief: Argo CD's rollout of the workspaces is named in its notes (controller, syncs ws-*;
ApplicationSet) instead of a line, which would have crossed Keycloak. The egress is one edge from the cluster to
Cloud NAT, and each internet service names its caller (conde, Keycloak, Argo CD) in its note. Microsoft Entra ID has
no product icon in the Azure pack V24, so the Dakai tenant uses the official App Registrations icon (dakai signs in
through an app registration); Slack has no icon in Simple Icons and gets the kit's neutral cloud glyph.

Not verified: that the estate is applied as this commit describes it (no read of the live project or cluster), and
that `s4-iap` is merged (it is not on master at the time of drawing).

Checks: tiles 3 x 2 clean for B, silhouette shows the top band, the edge stripe, the VPC with the three namespace
columns, the managed services and the right column. Label grep clean: every must word present, no Azure,
nativeai, stconde, date, em or en dash or semicolon. `plate_check.py` on the A4 landscape default: FAIL, median
label 3.9 pt; the drawing is a screen and large-format picture for review, not an A4 plate (the PDF page is
2175 x 1482 pt, labels at 12 pt).
