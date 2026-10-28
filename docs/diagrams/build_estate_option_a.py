#!/usr/bin/env python3
"""conde.cloud estate, option A (not picked, kept for the comparison in REVIEW.md): zones as columns.

People and Cloudflare at the left; the GCP project in the middle (the internet edge column, then the VPC with the
GKE cluster whose namespaces are rows: Argo CD, Keycloak, the workspaces; managed services under the VPC); the
services conde and Keycloak call, and GitHub, at the right, reached through Cloud NAT.
"""
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from diagramkit import *  # noqa: E402,F401,F403

set_palette("kit-blue")
BRAND.update({"cloudflare": "#F38020", "cloudflarepages": "#F38020", "github": "#181717", "githubactions": "#2088FF",
              "argo": "#EF7B4D", "keycloak": "#4D4D4D", "resend": "#000000", "google": "#4285F4",
              "terraform": "#844FBA"})

APP, EXT, DATA, DEPLOY = C["app"], C["carrier"], C["data"], C["deploy"]
DASH = "6 4"
LS, NS = 16, 14.5
ICON = 44
half = ICON / 2

W = 2900
M = 54
# Columns
LX0, LX1 = M, 384                        # people, Cloudflare, legend
PX0, PX1 = 424, 2464                     # GCP project
RX0, RX1 = 2524, W - M                   # internet services, GitHub
BUS = 2494                               # egress bus between the project and the right column
LBX0, LBX1 = PX0 + 24, PX0 + 24 + 200    # load balancer box
ECX0, ECX1 = LBX1 + 24, LBX1 + 24 + 286  # policy cards
VX0, VX1 = ECX1 + 32, PX1 - 24           # VPC
CX0, CX1 = VX0 + 24, 2214                # cluster
IX0, IX1 = CX0 + 24, CX1 - 24            # namespace rows
NAT_CX = (CX1 + VX1) / 2

# Rows
PY0 = 150
VY0 = PY0 + 64
CY0 = VY0 + 58
R1Y, R1H = CY0 + 70, 176                 # Argo CD
R2Y, R2H = R1Y + R1H + 24, 290           # Keycloak
GAP3 = 74                                # room for the workspace to Keycloak edges
R3Y, R3H = R2Y + R2H + GAP3, 330         # workspaces
CY1 = R3Y + R3H + 24
PSC_Y = CY1 + 64
VY1 = PSC_Y + ICON + 30
MY0 = VY1 + 34                           # managed services
MH = 262
MY1 = MY0 + MH
PY1 = MY1 + 24
H = PY1 + 50

header(W, "conde.cloud estate", "GCP project conde-prod in europe-west3, Cloudflare for DNS and the site, "
       "and the services the estate calls", "conde · Infrastructure")


def host(x, y, value, anchor="start", size=13.5):
    mono(x, y, value, size, C["navy"], 600, anchor)


def text_end(x, rows, label=None):
    """Right end of an hnode's text block whose icon starts at x."""
    w = max([tw(r_, NS) for r_ in rows] + ([tw(label, LS, 650)] if label else []))
    return x + ICON + 12 + w


# ── Trust boundaries ─────────────────────────────────────────────────────────────────────────────────────────────
account_box(PX0, PY0, PX1 - PX0, PY1 - PY0, "GCP project conde-prod", "gcp:project")
text(PX1 - 20, PY0 + 31, "europe-west3, Terraform roots in infra/gcp", 14, 500, C["muted"], "end")
boundary(VX0, VY0, VX1 - VX0, VY1 - VY0, "VPC conde", "subnet gke-europe-west3 10.10.0.0/20, private nodes",
         C["vpc_fill"], C["vpc"], "gcp:virtual_private_cloud")
cluster_box(CX0, CY0, CX1 - CX0, CY1 - CY0, "GKE Autopilot conde",
            "Dataplane V2, deny-by-default NetworkPolicy, Secret Manager CSI add-on",
            "gcp:google_kubernetes_engine")

# ── Left column: the owner, a teammate, Cloudflare, legend ───────────────────────────────────────────────────────
TF_CY = CY0 + 36                          # Terraform lines up with the GKE DNS endpoint
OB_CY = R1Y + 112                         # the owner's browser, into the load balancer
OW_Y = PY0
OW_H = OB_CY + half + 34 - OW_Y
card(LX0, OW_Y, LX1 - LX0, OW_H, "Owner")
hnode(LX0 + 20, OW_Y + 54, "glyph:users", "Owner", "kdobmayer.devops@gmail.com", ICON, LS, 13.5)
TF_ROWS = ["as terraform@conde-prod", "state in conde-tfstate"]
hnode(LX0 + 20, TF_CY - half, "si:terraform", "Terraform", TF_ROWS, ICON, LS, NS)
hnode(LX0 + 20, OB_CY - half, "glyph:browser", "Browser", ["Keycloak console, Argo CD"], ICON, LS, NS)

TM_H, CF_H = 300, 410
LGAP = (MY1 - (OW_Y + OW_H) - TM_H - CF_H) / 2
TM_Y = OW_Y + OW_H + LGAP
card(LX0, TM_Y, LX1 - LX0, TM_H, "Teammate")
TM_CY = TM_Y + 84
hnode(LX0 + 20, TM_CY - half, "glyph:users", "conde CLI and agent", ["board, MCP server, skill"], ICON, LS, NS)
TB_CY = TM_CY + 84
hnode(LX0 + 20, TB_CY - half, "glyph:browser", "Browser", ["web app, sign-in"], ICON, LS, NS)
TI_CY = TB_CY + 84
hnode(LX0 + 20, TI_CY - half, "glyph:file", "Installer", ["install.sh, then the binary", "from conde-cloud-dist"], ICON, LS, NS)

CF_Y = TM_Y + TM_H + LGAP
section(LX0, CF_Y, LX1 - LX0, CF_H, "Zone conde.cloud", "Cloudflare", "#FFFFFF", C["sec_ext"])
PG_Y = CF_Y + 82
hnode(LX0 + 20, PG_Y, "si:cloudflarepages", "Pages conde-cloud", ["conde.cloud and www, proxied:", "site, docs, install.sh"],
      ICON, LS, NS)
hnode(LX0 + 20, PG_Y + 104, "si:cloudflare", "DNS records", ["*.conde.cloud A record to the", "Gateway, DNS only, and the",
                                                             "certificate authorization"], ICON, LS, NS)
hnode(LX0 + 20, PG_Y + 218, "glyph:mail", "Mail records", ["Resend DKIM, SPF and MX", "on send, DMARC"], ICON, LS, NS)

# ── Internet edge column ──────────────────────────────────────────────────────────────────────────────────────────
DNS_ROWS = ["IAM only, for Terraform and", "the port-forward to Keycloak"]
hnode(ECX0, TF_CY - half, "gcp:google_kubernetes_engine", "GKE DNS endpoint", DNS_ROWS, ICON, LS, NS)

# Admin hosts: Cloud Armor throttles, then IAP. Exits: Argo CD (row 1), keycloak-admin (row 2, upper).
ARGO_CY = R1Y + 100
KA_CY = R2Y + 70                           # keycloak-admin entry
KP_CY = R2Y + 222                          # keycloak entry
AD_Y0, AD_Y1 = R1Y, KA_CY + 66
card(ECX0, AD_Y0, ECX1 - ECX0, AD_Y1 - AD_Y0, "Admin hosts", fill="#FFFFFF")
hnode(ECX0 + 16, AD_Y0 + 54, "gcp:cloud_armor", "Cloud Armor", ["conde-argocd and", "conde-keycloak-admin:", "per-IP throttle"],
      ICON, LS, NS)
hnode(ECX0 + 16, AD_Y0 + 178, "gcp:identity-aware_proxy", "Identity-Aware Proxy", ["only the owner's", "Google account passes"],
      ICON, LS, NS)
host(ECX0 + 16, AD_Y1 - 50, "argocd.conde.cloud", size=13)
host(ECX0 + 16, AD_Y1 - 28, "admin-auth.conde.cloud", size=13)
# Public hosts: Cloud Armor conde-web. Exits: Keycloak (row 2, lower), workspaces (row 3).
WS_IN = R3Y + 66
PU_Y0, PU_Y1 = KP_CY - 70, WS_IN + 100
card(ECX0, PU_Y0, ECX1 - ECX0, PU_Y1 - PU_Y0, "Public hosts", fill="#FFFFFF")
hnode(ECX0 + 16, PU_Y0 + 70, "gcp:cloud_armor", "Cloud Armor", ["conde-web: admin and", "master paths denied,",
                                                                "sign-in rate ban,", "L7 DDoS defense"], ICON, LS, NS)
host(ECX0 + 16, PU_Y1 - 50, "auth.conde.cloud", size=13)
host(ECX0 + 16, PU_Y1 - 28, "<workspace>.conde.cloud", size=13)

LBY0, LBY1 = R1Y, PU_Y1
rect(LBX0, LBY0, LBX1 - LBX0, LBY1 - LBY0, C["web_fill"], C["web_stroke"], 1.4, 12)
lb_cx = (LBX0 + LBX1) / 2
step = (LBY1 - LBY0) / 3
node(lb_cx, LBY0 + 34, "gcp:cloud_load_balancing", ["Gateway conde"],
     ["global external", "Application Load", "Balancer, HTTPS", "and HTTP redirect"], 48, lsize=LS, nsize=NS)
node(lb_cx, LBY0 + step + 70, "gcp:certificate_manager", ["Certificate", "Manager"], ["conde.cloud and", "*.conde.cloud"],
     48, lsize=LS, nsize=NS)
node(lb_cx, LBY0 + 2 * step + 80, "gcp:cloud_external_ip_addresses", ["Static IP"], ["conde-gateway"], 48,
     lsize=LS, nsize=NS)

# ── Cluster row 1: Argo CD ────────────────────────────────────────────────────────────────────────────────────────
card(IX0, R1Y, IX1 - IX0, R1H, "Argo CD", note="namespace argocd, chart argo-cd 10.10.3")
image(IX0 + 18, ARGO_CY - 20, "si:argo", 40)
lines_at(IX0 + 70, ARGO_CY - 4, "argocd-server", None, LS, NS)
host(IX0 + 70, ARGO_CY + 18, "argocd.conde.cloud")
px = IX0 + 318
for i, (lab, note) in enumerate([("server", "UI, API"), ("repo-server", "renders charts"),
                                 ("controller", "syncs ws-*"), ("applicationset", "app per tenant"),
                                 ("redis", "cache")]):
    glyph_pod(px + i * 116, R1Y + 54, lab, note, "box", r=20, lsize=14, nsize=12.5)
lines_at(IX1 - 296, R1Y + 72, "ApplicationSet conde-workspaces", ["one ws-<name> per deploy/tenants file,",
                                                                 "image from deploy/image.yaml,",
                                                                 "sign-in: realm infra, infra-admins"], LS, NS)

# ── Cluster row 2: Keycloak ───────────────────────────────────────────────────────────────────────────────────────
card(IX0, R2Y, IX1 - IX0, R2H, "Keycloak", note="namespace keycloak, one installation")
image(IX0 + 18, KA_CY - 20, "si:keycloak", 40)
lines_at(IX0 + 70, KA_CY - 4, "Service keycloak-admin", None, LS, NS)
host(IX0 + 70, KA_CY + 18, "admin-auth.conde.cloud")
text(IX0 + 70, KA_CY + 38, "console, realm master", 13.5, 400, C["muted"])
image(IX0 + 18, KP_CY - 20, "si:keycloak", 40)
lines_at(IX0 + 70, KP_CY - 4, "Service keycloak", None, LS, NS)
host(IX0 + 70, KP_CY + 18, "auth.conde.cloud")
text(IX0 + 70, KP_CY + 38, "workspace realms, infra", 13.5, 400, C["muted"])
kc_px = IX0 + 390
glyph_pod(kc_px, R2Y + 66, "Keycloak 26.8.1", ["1 replica, conde theme,", "secrets and file vault", "as CSI files"], "box",
          lsize=LS, nsize=NS)
glyph_pod(kc_px + 222, R2Y + 66, "Cloud SQL Auth Proxy", ["sidecar, password", "auth to keycloak"], "link", r=19,
          lsize=LS, nsize=NS)
RL_X = IX1 - 320
text(RL_X, R2Y + 66, "REALMS", 12.5, 700, C["card_title"], spacing=1.0)
for i, (name, note) in enumerate([("demo", "passwords, invites"), ("clearscale", "Google, clearscale.com"),
                                  ("infra", "Argo CD sign-in, MFA"), ("master", "console, Terraform, MFA")]):
    yy = R2Y + 84 + i * 38
    chip(RL_X, yy, name, C["navy"], size=12)
    text(RL_X + 110, yy + 16, note, NS, 400, C["muted"])
text(RL_X, R2Y + R2H - 14, "mail through Resend SMTPS", 13.5, 400, C["muted"])

# ── Cluster row 3: workspaces ─────────────────────────────────────────────────────────────────────────────────────
section(IX0, R3Y, IX1 - IX0, R3H, "One namespace per workspace", "Workspaces, charts/conde-workspace",
        "#FFFFFF", C["cluster"], kcolor=C["cluster"])
WS = [("ws-dakai", "dakai.conde.cloud", ["sign-in: Dakai Entra tenant", "database conde_dakai"]),
      ("ws-demo", "demo.conde.cloud", ["sign-in: realm demo", "database conde_demo"]),
      ("ws-clearscale", "clearscale.conde.cloud", ["sign-in: realm clearscale", "database conde_clearscale"])]
WCW = (IX1 - IX0 - 2 * 18 - 2 * 18) / 3
WC_Y = R3Y + 74
WC_H = R3H - 74 - 18
ws_cx = []
for i, (ns, hostname, notes) in enumerate(WS):
    x = IX0 + 18 + i * (WCW + 18)
    card(x, WC_Y, WCW, WC_H, ns, fill=C["web_fill"], stroke=C["web_stroke"])
    host(x + WCW - 14, WC_Y + 24, hostname, "end", 13)
    ws_cx.append(x + WCW / 2)
    glyph_pod(x + 70, WC_Y + 54, "conde", "2 replicas", "box", stack=1, lsize=LS, nsize=NS)
    glyph_pod(x + 70, WC_Y + 152, "Auth Proxy", "IAM database auth", "link", r=17, lsize=14, nsize=12.5)
    lines_at(x + 150, WC_Y + 74, "Secrets as CSI files", ["admin key, OIDC client", "secret, Slack, Resend"], 15, 13.5)
    lines_at(x + 150, WC_Y + 164, notes[0], [notes[1]], 15, 13.5)

# ── VPC: Cloud NAT and the Private Service Connect endpoint ──────────────────────────────────────────────────────
NAT_CY = ARGO_CY
node(NAT_CX, NAT_CY - 24, "gcp:cloud_nat", "Cloud NAT", ["egress of", "every pod"], 48, lsize=LS, nsize=NS)
PSC_X = CX0 + 400
PSC_ROWS = ["psc-conde-pg, private DNS zone"]
hnode(PSC_X - half, PSC_Y, "gcp:private_service_connect", "Private Service Connect", PSC_ROWS, ICON, LS, NS)

# ── Managed services under the VPC ───────────────────────────────────────────────────────────────────────────────
DA_X0, DA_X1 = PX0 + 24, PSC_X + 220
section(DA_X0, MY0, DA_X1 - DA_X0, MH, "Data", "Managed services", "#FFFFFF", C["sec_data"], kcolor=C["kick_data"])
MN_Y = MY0 + 84
SM_X = CX0 + 40
TS_X = SM_X - 360
node(TS_X, MN_Y, "gcp:cloud_storage", "conde-tfstate", ["Terraform state,", "one prefix per root"], 48, lsize=LS, nsize=NS)
node(SM_X, MN_Y, "gcp:secret_manager", "Secret Manager", ["generated values written", "write-only, read as files"],
     48, lsize=LS, nsize=NS)
node(PSC_X, MN_Y, "gcp:cloud_sql", "Cloud SQL conde-pg", ["PostgreSQL 18, zonal, PITR,", "no public IP, IAM auth on,",
                                                         "keycloak, conde_<workspace>"], 48, lsize=LS, nsize=NS)

DL_X0, DL_X1 = DA_X1 + 24, PX1 - 24
section(DL_X0, MY0, DL_X1 - DL_X0, MH, "Delivery", "Release", "#FFFFFF", C["legend"], kcolor=C["legend"])
AR_X = DL_X0 + 170
AR_Y = MY0 + 66
AR_ROWS = ["images conde, keycloak,", "20 newest kept"]
hnode(AR_X - half, AR_Y, "gcp:artifact_registry", "Artifact Registry conde", AR_ROWS, ICON, LS, NS)
DI_Y = AR_Y + 104
DI_ROWS = ["public read: binaries,", "SHA256SUMS, latest"]
hnode(AR_X - half, DI_Y, "gcp:cloud_storage", "conde-cloud-dist", DI_ROWS, ICON, LS, NS)
WIF_ROWS = ["Federation, pool github,", "service account deploy"]
WIF_X = DL_X1 - 24 - (ICON + 12 + tw(WIF_ROWS[0], NS))
WIF_CY = (AR_Y + DI_Y) / 2 + half
hnode(WIF_X, WIF_CY - half, "gcp:workload_identity_pool", "Workload Identity", WIF_ROWS, ICON, LS, NS)

# ── Right column: internet services and GitHub ───────────────────────────────────────────────────────────────────
EXTS = [("az:App-Registrations", "Dakai Entra tenant", ["conde of dakai: OIDC", "sign-in, app registration"]),
        ("si:google", "Google", ["Keycloak: clearscale sign-in,", "also the IAP sign-in"]),
        ("si:resend", "Resend", ["Keycloak: SMTPS 465,", "conde: invite email API"]),
        ("glyph:cloud", "Slack", ["conde: bot messages"])]
EXT_Y0 = PY0
EX_STEP = 140
EX_H = 86 + (len(EXTS) - 1) * EX_STEP + ICON + 40
GH_H = 360
GH_Y0 = MY1 - GH_H
LG_H = 236
LG_Y = (EXT_Y0 + EX_H + GH_Y0 - LG_H) / 2
section(RX0, EXT_Y0, RX1 - RX0, EX_H, "Called through Cloud NAT", "Internet", "#FFFFFF", C["sec_carrier"],
        kcolor=C["kick_carrier"])
legend(RX0, LG_Y, RX1 - RX0, LG_H, [("Requests from the internet", "app"), ("Calls out, through Cloud NAT", "carrier"),
                                     ("Database and secrets", "data"), ("Build, deploy, Terraform", "deploy"),
                                     ("Trust boundary", "boundary")])
ext_cy = []
for i, (ico, lab, note) in enumerate(EXTS):
    y = EXT_Y0 + 86 + i * EX_STEP
    hnode(RX0 + 26, y, ico, lab, note, ICON, LS, NS)
    ext_cy.append(y + half)

section(RX0, GH_Y0, RX1 - RX0, GH_H, "kdobmayer/conde", "GitHub", "#FFFFFF", C["legend"], kcolor=C["legend"])
REPO_Y = GH_Y0 + 90
hnode(RX0 + 26, REPO_Y, "si:github", "Repository", ["Argo CD pulls it over SSH:", "charts, deploy/tenants,",
                                                    "deploy/image.yaml on main"], ICON, LS, NS)
ACT_CY = WIF_CY
hnode(RX0 + 26, ACT_CY - half, "si:githubactions", "GitHub Actions", ["release on a v* tag,", "site to Cloudflare Pages"],
      ICON, LS, NS)

# ── Edges ────────────────────────────────────────────────────────────────────────────────────────────────────────
# Requests: people to the load balancer, the load balancer to the policies, the policies to the namespaces.
edge([(LX1, OB_CY), (LBX0, OB_CY)], APP)
edge([(LX1, TM_CY), (LBX0, TM_CY)], APP)
edge([(LX1, TB_CY), (LBX0, TB_CY)], APP)
IS_X = LX1 - 60
edge([(IS_X, TM_Y + TM_H), (IS_X, CF_Y)], APP, "install.sh", label_at=(IS_X - 12, TM_Y + TM_H + LGAP / 2 + 5),
     anchor="end")
LB_AD = AD_Y0 + 120
LB_PU = PU_Y0 + 120
edge([(LBX1, LB_AD), (ECX0, LB_AD)], APP)
edge([(LBX1, LB_PU), (ECX0, LB_PU)], APP)
edge([(ECX1, ARGO_CY), (IX0, ARGO_CY)], APP)
edge([(ECX1, KA_CY), (IX0, KA_CY)], APP)
edge([(ECX1, KP_CY), (IX0, KP_CY)], APP)
edge([(ECX1, WS_IN), (IX0, WS_IN)], APP)

# Terraform through the GKE DNS endpoint: kubernetes and helm providers, the port-forward to Keycloak for the realms.
edge([(text_end(LX0 + 20, TF_ROWS) + 10, TF_CY), (ECX0 - 6, TF_CY)], DEPLOY, dash=DASH)
edge([(text_end(ECX0, DNS_ROWS, "GKE DNS endpoint") + 10, TF_CY), (CX0, TF_CY)], DEPLOY, dash=DASH)

# conde to Keycloak: OIDC through auth.conde.cloud, and the admin API of demo.
for i, lab in ((1, "OIDC, admin API"), (2, "OIDC")):
    x = ws_cx[i]
    edge([(x, WC_Y), (x, R2Y + R2H)], EXT, lab, label_at=(x + 12, R2Y + R2H + GAP3 / 2 + 6), anchor="start")

# Egress through Cloud NAT to the internet services and GitHub.
edge([(CX1, NAT_CY), (NAT_CX - 30, NAT_CY)], EXT)
for cy in ext_cy + [REPO_Y + half]:
    edge([(NAT_CX + 30, NAT_CY), (BUS, NAT_CY), (BUS, cy), (RX0 + 20, cy)], EXT)

# Database and secrets.
edge([(PSC_X, CY1), (PSC_X, PSC_Y - 6)], DATA, "Auth Proxy sidecars", label_at=(PSC_X + 12, CY1 + 36), anchor="start")
edge([(PSC_X, PSC_Y + ICON + 6), (PSC_X, MN_Y - 6)], DATA)
edge([(SM_X, CY1), (SM_X, MN_Y - 6)], DATA, "CSI files", label_at=(SM_X + 12, CY1 + 36), anchor="start")

# Delivery.
edge([(AR_X, AR_Y - 6), (AR_X, CY1)], DEPLOY, "image pull", dash=DASH, label_at=(AR_X + 12, CY1 + 36), anchor="start")
edge([(RX0 + 20, ACT_CY), (text_end(WIF_X, WIF_ROWS, "Workload Identity") + 10, ACT_CY)], DEPLOY, dash=DASH)
label(BUS, ACT_CY - 10, "OIDC", DEPLOY)
ar_end = text_end(AR_X - half, AR_ROWS, "Artifact Registry conde") + 10
di_end = text_end(AR_X - half, DI_ROWS, "conde-cloud-dist") + 10
JX = WIF_X - 44
edge([(WIF_X - 6, WIF_CY - 10), (JX, WIF_CY - 10), (JX, AR_Y + half), (ar_end, AR_Y + half)], DEPLOY,
     "image", dash=DASH, label_at=((ar_end + JX) / 2, AR_Y + half - 10))
edge([(WIF_X - 6, WIF_CY + 10), (JX, WIF_CY + 10), (JX, DI_Y + half), (di_end, DI_Y + half)], DEPLOY,
     "binaries", dash=DASH, label_at=((di_end + JX) / 2, DI_Y + half - 10))
ax = RX0 + 26 + half
edge([(ax, ACT_CY - half - 6), (ax, REPO_Y + ICON + 6)], DEPLOY, "promote", dash=DASH,
     label_at=(ax + 12, (REPO_Y + ICON + ACT_CY - half) / 2 + 5), anchor="start")

write("estate-option-a", W, H)
