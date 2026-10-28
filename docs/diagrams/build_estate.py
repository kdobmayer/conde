#!/usr/bin/env python3
"""conde.cloud estate (the picked layout, option B): zones as stripes.

The owner, a teammate and Cloudflare in a band on top; below it the GCP project as stripes read downwards: the
internet edge (load balancer bar, then the public and the admin policies), the VPC with the GKE cluster whose
namespaces stand as columns (workspaces, Keycloak, Argo CD), managed services at the bottom. The services the
estate calls, and GitHub, are at the right, reached through Cloud NAT.
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
PX0, PX1 = M, 2464                       # top band and GCP project
RX0, RX1 = 2524, W - M                   # internet services, legend, GitHub
BUS = 2494
VX0, VX1 = PX0 + 24, PX1 - 24            # VPC
CX0, CX1 = VX0 + 24, 2214                # cluster
NAT_CX = (CX1 + VX1) / 2
# Namespace columns
WSX0, WSX1 = CX0 + 24, 1250
KCX0, KCX1 = WSX1 + 90, 1840
AGX0, AGX1 = KCX1 + 24, CX1 - 24
# Edge stripe columns
DNX0, DNX1 = 330, 600                    # GKE DNS endpoint
LBX0, LBX1 = DNX1 + 24, VX1              # load balancer bar
PUX0, PUX1 = LBX0, 1640                  # public hosts
ADX0, ADX1 = PUX1 + 24, VX1              # admin hosts

# Rows
TB_Y, TB_H = 150, 170                    # top band
PY0 = TB_Y + TB_H + 46
ES_Y0 = PY0 + 64                         # edge stripe
LB_H = 96
PC_Y0 = ES_Y0 + LB_H + 40                # policy cards
PC_H = 156
ES_Y1 = PC_Y0 + PC_H
VY0 = ES_Y1 + 40
CY0 = VY0 + 58
COL_Y = CY0 + 64
COL_H = 560
CY1 = COL_Y + COL_H + 24
PSC_Y = CY1 + 64
VY1 = PSC_Y + ICON + 30
MY0 = VY1 + 34
MH = 262
MY1 = MY0 + MH
PY1 = MY1 + 24
H = PY1 + 50

header(W, "conde.cloud estate", "GCP project conde-prod in europe-west3, Cloudflare for DNS and the site, "
       "and the services the estate calls", "conde · Infrastructure")


def host(x, y, value, anchor="start", size=13.5):
    mono(x, y, value, size, C["navy"], 600, anchor)


def text_end(x, rows, label=None):
    w = max([tw(r_, NS) for r_ in rows] + ([tw(label, LS, 650)] if label else []))
    return x + ICON + 12 + w


# ── Top band: owner, teammate, Cloudflare ────────────────────────────────────────────────────────────────────────
IT_Y = TB_Y + 70                          # item icon top in the band
OWX0, OWX1 = PX0, 870
card(OWX0, TB_Y, OWX1 - OWX0, TB_H, "Owner", note="kdobmayer.devops@gmail.com")
TF_X = 380                          # Terraform sits over the GKE DNS endpoint
TF_ROWS = ["as terraform@conde-prod,", "state in conde-tfstate"]
hnode(TF_X - half, IT_Y, "si:terraform", "Terraform", TF_ROWS, ICON, LS, NS)
OB_X = 690
hnode(OWX0 + 20, IT_Y, "glyph:users", "Owner", ["runs Terraform,", "admin consoles"], ICON, LS, NS)
hnode(OB_X - half, IT_Y, "glyph:browser", "Browser", ["console, Argo CD"], ICON, LS, NS)

TMX0, TMX1 = OWX1 + 24, OWX1 + 24 + 724
card(TMX0, TB_Y, TMX1 - TMX0, TB_H, "Teammate")
TC_X = TMX0 + 44
hnode(TC_X - half, IT_Y, "glyph:users", "conde CLI and agent", ["board, MCP server, skill"], ICON, LS, NS)
TB_X = TMX0 + 290
hnode(TB_X - half, IT_Y, "glyph:browser", "Browser", ["web app, sign-in"], ICON, LS, NS)
TI_X = TMX0 + 480
TI_ROWS = ["install.sh, then the binary", "from conde-cloud-dist"]
hnode(TI_X - half, IT_Y, "glyph:file", "Installer", TI_ROWS, ICON, LS, NS)

CFX0, CFX1 = TMX1 + 54, PX1
card(CFX0, TB_Y, CFX1 - CFX0, TB_H, "Cloudflare, zone conde.cloud")
PG_X = CFX0 + 44
hnode(PG_X - half, IT_Y, "si:cloudflarepages", "Pages conde-cloud", ["conde.cloud, www, proxied:", "site, docs, install.sh"],
      ICON, LS, NS)
hnode(PG_X + 285 - half, IT_Y, "si:cloudflare", "DNS records", ["*.conde.cloud A record to the", "Gateway, DNS only"],
      ICON, LS, NS)
hnode(PG_X + 555 - half, IT_Y, "glyph:mail", "Mail records", ["Resend DKIM, SPF,", "MX, DMARC"], ICON, LS, NS)

# ── Trust boundaries ─────────────────────────────────────────────────────────────────────────────────────────────
account_box(PX0, PY0, PX1 - PX0, PY1 - PY0, "GCP project conde-prod", "gcp:project")
text(PX1 - 20, PY0 + 31, "europe-west3, Terraform roots in infra/gcp", 14, 500, C["muted"], "end")
boundary(VX0, VY0, VX1 - VX0, VY1 - VY0, "VPC conde", "subnet gke-europe-west3 10.10.0.0/20, private nodes",
         C["vpc_fill"], C["vpc"], "gcp:virtual_private_cloud")
cluster_box(CX0, CY0, CX1 - CX0, CY1 - CY0, "GKE Autopilot conde", None, "gcp:google_kubernetes_engine")
text(CX0 + 56 + tw("GKE Autopilot conde", 21, 700) + 24, CY0 + 36,
     "Dataplane V2, deny-by-default NetworkPolicy, Secret Manager CSI add-on", 14, 500, C["muted"])

# ── Edge stripe ──────────────────────────────────────────────────────────────────────────────────────────────────
DNS_ROWS = ["IAM only, no IP endpoint,", "Terraform: kubernetes, helm,", "port-forward to Keycloak"]
hnode(TF_X - half, ES_Y0 + 100, "gcp:google_kubernetes_engine", "GKE DNS endpoint", DNS_ROWS, ICON, LS, NS)

rect(LBX0, ES_Y0, LBX1 - LBX0, LB_H, C["web_fill"], C["web_stroke"], 1.4, 12)
LB_IY = ES_Y0 + (LB_H - 48) / 2
hnode(LBX0 + 24, LB_IY, "gcp:cloud_load_balancing", "Gateway conde",
      ["global external Application Load Balancer,", "HTTPS, HTTP redirects"], 48, LS, NS)
hnode(LBX0 + 560, LB_IY, "gcp:certificate_manager", "Certificate Manager", ["conde.cloud and *.conde.cloud"], 48, LS, NS)
hnode(LBX0 + 920, LB_IY, "gcp:cloud_external_ip_addresses", "Static IP conde-gateway", ["one global address"], 48, LS, NS)

card(PUX0, PC_Y0, PUX1 - PUX0, PC_H, "Public hosts", fill="#FFFFFF")
hnode(PUX0 + 16, PC_Y0 + 56, "gcp:cloud_armor", "Cloud Armor conde-web",
      ["admin and master paths denied,", "sign-in rate ban, L7 DDoS defense"], ICON, LS, NS)
host(PUX1 - 16, PC_Y0 + 76, "<workspace>.conde.cloud", "end")
host(PUX1 - 16, PC_Y0 + 100, "auth.conde.cloud", "end")
card(ADX0, PC_Y0, ADX1 - ADX0, PC_H, "Admin hosts", fill="#FFFFFF")
hnode(ADX0 + 16, PC_Y0 + 56, "gcp:cloud_armor", "Cloud Armor", ["conde-keycloak-admin,", "conde-argocd: per-IP throttle"],
      ICON, LS, NS)
hnode(ADX0 + 330, PC_Y0 + 56, "gcp:identity-aware_proxy", "Identity-Aware Proxy", ["only the owner's Google", "account passes"],
      ICON, LS, NS)

# ── Namespace columns ────────────────────────────────────────────────────────────────────────────────────────────
section(WSX0, COL_Y, WSX1 - WSX0, COL_H, "One namespace per workspace", "Workspaces, charts/conde-workspace",
        "#FFFFFF", C["cluster"], kcolor=C["cluster"])
WS = [("ws-dakai", "dakai.conde.cloud", ["Dakai Entra tenant", "database conde_dakai"]),
      ("ws-demo", "demo.conde.cloud", ["realm demo", "database conde_demo"]),
      ("ws-clearscale", "clearscale.conde.cloud", ["realm clearscale", "database conde_clearscale"])]
WCW = (WSX1 - WSX0 - 2 * 18 - 2 * 18) / 3
WC_Y = COL_Y + 74
WC_H = COL_H - 74 - 18
for i, (ns, hostname, notes) in enumerate(WS):
    x = WSX0 + 18 + i * (WCW + 18)
    cx = x + WCW / 2
    card(x, WC_Y, WCW, WC_H, ns, fill=C["web_fill"], stroke=C["web_stroke"])
    host(cx, WC_Y + 64, hostname, "middle", 13.5)
    glyph_pod(cx - 70, WC_Y + 90, "conde", "2 replicas", "box", stack=1, lsize=LS, nsize=NS)
    glyph_pod(cx + 80, WC_Y + 96, "Auth Proxy", ["IAM database", "auth"], "link", r=17, lsize=14, nsize=12.5)
    yy = lines_at(x + 20, WC_Y + 244, "Secrets as CSI files", ["admin key, OIDC client secret,", "Slack token, Resend key"],
                  15, 13.5)
    lines_at(x + 20, yy + 34, "Sign-in", [notes[0]], 15, 13.5)
    lines_at(x + 20, yy + 112, "Database", [notes[1].replace("database ", "")], 15, 13.5)

card(KCX0, COL_Y, KCX1 - KCX0, COL_H, "Keycloak")
KP_X = KCX0 + 130                          # Service keycloak entry, from the public hosts
KA_X = KCX0 + 380                          # Service keycloak-admin entry, from the admin hosts
image(KP_X - 20, COL_Y + 52, "si:keycloak", 40)
lines_at(KP_X, COL_Y + 118, "Service keycloak", None, LS, NS, "middle")
host(KP_X, COL_Y + 140, "auth.conde.cloud", "middle", 13)
text(KP_X, COL_Y + 160, "workspace realms, infra", 13.5, 400, C["muted"], "middle")
image(KA_X - 20, COL_Y + 52, "si:keycloak", 40)
lines_at(KA_X, COL_Y + 118, "Service keycloak-admin", None, LS, NS, "middle")
host(KA_X, COL_Y + 140, "admin-auth.conde.cloud", "middle", 13)
text(KA_X, COL_Y + 160, "console, realm master", 13.5, 400, C["muted"], "middle")
glyph_pod(KP_X, COL_Y + 196, "Keycloak 26.8.1", ["1 replica, conde theme,", "secrets, file vault as", "CSI files"], "box",
          lsize=LS, nsize=NS)
glyph_pod(KA_X, COL_Y + 200, "Cloud SQL Auth Proxy", ["sidecar, password", "auth to keycloak"], "link", r=19,
          lsize=LS, nsize=NS)
RL_X = KCX0 + 24
text(RL_X, COL_Y + 400, "REALMS", 12.5, 700, C["card_title"], spacing=1.0)
for i, (name, note) in enumerate([("demo", "passwords, invites"), ("clearscale", "Google, clearscale.com"),
                                  ("infra", "Argo CD sign-in, MFA"), ("master", "console, Terraform, MFA")]):
    yy = COL_Y + 414 + i * 32
    chip(RL_X, yy, name, C["navy"], size=12)
    text(RL_X + 110, yy + 16, note, NS, 400, C["muted"])
text(KCX1 - 20, COL_Y + 400, "mail: Resend SMTPS", 13.5, 400, C["muted"], "end")

card(AGX0, COL_Y, AGX1 - AGX0, COL_H, "Argo CD", note="chart 10.10.3")
AG_X = (AGX0 + AGX1) / 2
image(AG_X - 20, COL_Y + 52, "si:argo", 40)
lines_at(AG_X, COL_Y + 118, "argocd-server", None, LS, NS, "middle")
host(AG_X, COL_Y + 140, "argocd.conde.cloud", "middle", 13)
text(AG_X, COL_Y + 160, "namespace argocd", 13.5, 400, C["muted"], "middle")
pods = [("server", "UI, API"), ("repo-server", "renders charts"), ("controller", "syncs ws-*"),
        ("applicationset", "app per tenant"), ("redis", "cache")]
for i, (lab, note) in enumerate(pods):
    row, col = divmod(i, 3)
    n = 3 if row == 0 else 2
    step = (AGX1 - AGX0) / (n + 0.0)
    gx = AGX0 + step / 2 + col * step
    glyph_pod(gx, COL_Y + 200 + row * 110, lab, note, "box", r=18, lsize=14, nsize=12.5)
lines_at(AGX0 + 20, COL_Y + 456, "ApplicationSet", ["conde-workspaces: one ws-<name>", "per deploy/tenants file,",
                                                    "sign-in: realm infra"], LS, NS)

# ── VPC: Cloud NAT and Private Service Connect ───────────────────────────────────────────────────────────────────
NAT_CY = COL_Y + 120
node(NAT_CX, NAT_CY - 24, "gcp:cloud_nat", "Cloud NAT", ["egress of", "every pod"], 48, lsize=LS, nsize=NS)
PSC_X = CX0 + 820
hnode(PSC_X - half, PSC_Y, "gcp:private_service_connect", "Private Service Connect",
      ["psc-conde-pg, private DNS zone"], ICON, LS, NS)

# ── Managed services ─────────────────────────────────────────────────────────────────────────────────────────────
DA_X0, DA_X1 = PX0 + 24, PSC_X + 240
section(DA_X0, MY0, DA_X1 - DA_X0, MH, "Data", "Managed services", "#FFFFFF", C["sec_data"], kcolor=C["kick_data"])
MN_Y = MY0 + 84
SM_X = PSC_X - 300
TS_X = SM_X - 300
node(TS_X, MN_Y, "gcp:cloud_storage", "conde-tfstate", ["Terraform state,", "one prefix per root"], 48, lsize=LS, nsize=NS)
node(SM_X, MN_Y, "gcp:secret_manager", "Secret Manager", ["generated values written", "write-only, read as files"],
     48, lsize=LS, nsize=NS)
node(PSC_X, MN_Y, "gcp:cloud_sql", "Cloud SQL conde-pg", ["PostgreSQL 18, zonal, PITR,", "no public IP, IAM auth on,",
                                                         "keycloak, conde_<workspace>"], 48, lsize=LS, nsize=NS)

DL_X0, DL_X1 = DA_X1 + 24, PX1 - 24
section(DL_X0, MY0, DL_X1 - DL_X0, MH, "Delivery", "Release", "#FFFFFF", C["legend"], kcolor=C["legend"])
AR_X = DL_X0 + 300
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

# ── Right column: internet services, legend, GitHub ──────────────────────────────────────────────────────────────
EXTS = [("az:App-Registrations", "Dakai Entra tenant", ["conde of dakai: OIDC", "sign-in, app registration"]),
        ("si:google", "Google", ["Keycloak: clearscale sign-in,", "also the IAP sign-in"]),
        ("si:resend", "Resend", ["Keycloak: SMTPS 465,", "conde: invite email API"]),
        ("glyph:cloud", "Slack", ["conde: bot messages"])]
EXT_Y0 = PY0
EX_STEP = 140
EX_H = 86 + (len(EXTS) - 1) * EX_STEP + ICON + 40
section(RX0, EXT_Y0, RX1 - RX0, EX_H, "Called through Cloud NAT", "Internet", "#FFFFFF", C["sec_carrier"],
        kcolor=C["kick_carrier"])
ext_cy = []
for i, (ico, lab, note) in enumerate(EXTS):
    y = EXT_Y0 + 86 + i * EX_STEP
    hnode(RX0 + 26, y, ico, lab, note, ICON, LS, NS)
    ext_cy.append(y + half)
GH_H = 360
GH_Y0 = MY1 - GH_H
LG_H = 236
LG_Y = (EXT_Y0 + EX_H + GH_Y0 - LG_H) / 2
legend(RX0, LG_Y, RX1 - RX0, LG_H, [("Requests from the internet", "app"), ("Calls out, through Cloud NAT", "carrier"),
                                     ("Database and secrets", "data"), ("Build, deploy, Terraform", "deploy"),
                                     ("Trust boundary", "boundary")])
section(RX0, GH_Y0, RX1 - RX0, GH_H, "kdobmayer/conde", "GitHub", "#FFFFFF", C["legend"], kcolor=C["legend"])
REPO_Y = GH_Y0 + 90
hnode(RX0 + 26, REPO_Y, "si:github", "Repository", ["Argo CD pulls it over SSH:", "charts, deploy/tenants,",
                                                    "deploy/image.yaml on main"], ICON, LS, NS)
ACT_CY = WIF_CY
hnode(RX0 + 26, ACT_CY - half, "si:githubactions", "GitHub Actions", ["release on a v* tag,", "site to Cloudflare Pages"],
      ICON, LS, NS)

# ── Edges ────────────────────────────────────────────────────────────────────────────────────────────────────────
it_bottom = IT_Y + ICON + 6
# Requests from the top band into the load balancer bar, then the policies, then the namespaces.
for x in (OB_X, TC_X, TB_X):
    edge([(x, it_bottom), (x, ES_Y0)], APP)
edge([(text_end(TI_X - half, TI_ROWS, "Installer") + 10, IT_Y + half), (PG_X - half - 6, IT_Y + half)], APP, "install.sh",
     label_at=((text_end(TI_X - half, TI_ROWS) + PG_X - half) / 2 + 4, IT_Y + half - 12))
P1, P2 = PUX0 + 300, ADX0 + 300
edge([(P1, ES_Y0 + LB_H), (P1, PC_Y0)], APP)
edge([(P2, ES_Y0 + LB_H), (P2, PC_Y0)], APP)
WS_IN = 1010
edge([(WS_IN, PC_Y0 + PC_H), (WS_IN, COL_Y)], APP)
edge([(KP_X, PC_Y0 + PC_H), (KP_X, COL_Y + 46)], APP)
edge([(KA_X, PC_Y0 + PC_H), (KA_X, COL_Y + 46)], APP)
edge([(AG_X, PC_Y0 + PC_H), (AG_X, COL_Y + 46)], APP)

# Terraform through the GKE DNS endpoint into the cluster: kubernetes, helm, the port-forward to Keycloak.
edge([(TF_X, it_bottom), (TF_X, ES_Y0 + 94)], DEPLOY, dash=DASH)
edge([(TF_X, ES_Y0 + 100 + ICON + 6), (TF_X, CY0)], DEPLOY, dash=DASH)

# conde to Keycloak: OIDC through auth.conde.cloud (demo, clearscale), the admin API of demo.
for y, lab in ((COL_Y + 300, "OIDC"), (COL_Y + 360, "admin API")):
    edge([(WSX1, y), (KCX0, y)], EXT, lab, label_at=((WSX1 + KCX0) / 2, y - 10))

# Egress through Cloud NAT.
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

write("conde-cloud-estate", W, H)
