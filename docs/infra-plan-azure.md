# conde on Azure Container Apps: inventory, proposed setup, cost, blockers (dakai-helper, 2026-10-09)

Planning only. Nothing here is created until Krisztián's go. Figures from the Azure retail price API (EUR,
West Europe, 2026-10-09) and from a stack of the same shape that went live yesterday (mhs-meetings,
terraform/meetings in DakaiGroup/native-infra), so the recipe below is tested, not theoretical.

## (a) Inventory that matters

Subscription "Azure subscription 1" 60df47fc-ce8f-4f52-a970-43a412d78007 (the Native subscription; the az CLI
default is Cockpit Dev, always pass `--subscription`). West Europe throughout.

| kind | existing | reuse for conde? |
|---|---|---|
| Container Apps environments | cae-native-meetings (rg-native-meetings; workload-profile env on VNet 10.140.0.0/16, Consumption profile; created 2026-10-08), cae-magicwand-demo (no VNet) | no: own environment, see (b). Reusing one couples conde to another service's lifecycle; an environment costs nothing on the Consumption profile |
| ACR | crnative (rg-native, Basic), dakaiacr (shared-rg, Standard), crdevopshub, crmagicwanddemo | crnative, image `crnative.azurecr.io/conde`; pull with the app's managed identity (AcrPull), push from GitHub via an OIDC identity (pattern in terraform/meetings/ci.tf) |
| Key Vault | kv-native, kv-native-hub*, kv-native-meetings, per tier vaults | own vault kv-conde (RBAC, no purge protection), Key Vault references from the app; cheaper to do on day one than "later" |
| DNS | Azure DNS: nativeai.space (+ dev/stage/judan/demo subzones). **dakai.io is NOT in Azure**: its name servers are Namecheap (dns1/dns2.registrar-servers.com) | see DNS below |
| Storage | stnativemeetings (Premium FileStorage, NFS share for mhs-meetings), stnative*, stnativehubstate (tfstate) | own account stconde (Premium FileStorage, one NFS share); Terraform state in stnativehubstate/tfstate like every root |
| Log Analytics | one per stack (log-native-meetings etc.) | own workspace log-conde, 30-day retention |

## (b) Proposed setup

One resource group rg-conde, one Terraform root (terraform/conde in native-infra, human-applied, excluded from
the tier pipelines, same as terraform/meetings). Resources:

- VNet vnet-conde 10.141.0.0/16 (10.140 is meetings; both outside the hub-and-spoke tiers), subnet snet-aca
  /23 delegated to Microsoft.App/environments with the Microsoft.Storage service endpoint. The VNet exists for
  one reason: NFS Azure Files mounts require a VNet-integrated, workload-profile environment.
- Environment cae-conde: workload profiles, Consumption profile only, Log Analytics logs. No NAT gateway
  (nothing needs a fixed egress IP; add later without recreation if ever needed).
- Storage account stconde: Premium FileStorage, LRS, shared-key auth disabled, secure transfer off (NFS needs
  it), network rules Deny + the snet-aca service endpoint + AzureServices bypass; one NFS 4.1 share `board`,
  100 GiB (the premium minimum). Registered on the environment as NFS storage, mounted into the app at
  /var/lib/conde (ReadWrite). No account key anywhere.
- Container App ca-conde: one container, image from crnative, 0.5 vCPU / 1 GiB (raise to 1/2 if the Rust
  server wants it), **min 1 = max 1** (single writer on the board; Single revision mode so a rollout never
  runs two writers for long, see blockers), external HTTPS ingress on the server's port, transport auto
  (HTTP/1.1 + HTTP/2, WebSocket upgrades pass through), ingress session affinity off (one replica).
  Liveness probe on a health path. Termination grace 60 s (max 600).
- Identity id-conde (user-assigned): AcrPull on crnative, Key Vault Secrets User on kv-conde.
- Secrets: kv-conde holds the API keys (and whatever conde needs); the app references them versionless and
  gets them as env vars or files under /etc/secrets. Developers who may write secrets get Key Vault Secrets
  Officer on kv-conde via an Entra group. Names unprefixed (one service per vault, Krisztián's convention).
- DNS + TLS: CNAME conde.<domain> -> the app's FQDN, TXT asuid.conde.<domain> -> the environment's
  verification id, then `az containerapp hostname bind --validation-method CNAME` issues a free managed
  certificate (5 to 15 minutes). Terraform ignores the certificate attributes afterwards.
  - conde.nativeai.space: fully automated, the zone is in Azure DNS (rg-native).
  - conde.dakai.io: Krisztián creates the two records at Namecheap once (or delegates a subzone to Azure
    DNS); everything else is the same. The owner picks the name.
- Release: GitHub Actions in the conde repo with an OIDC identity (AcrPush on crnative, Contributor on
  rg-conde): build, push, `az containerapp update --image`, then verify the new revision is healthy. No
  deploy keys, no secrets in the repo. Terraform ignores the image after bootstrap, so an apply never rolls a
  release back.
- Logs: stdout/stderr to Log Analytics (ContainerAppConsoleLogs_CL), `az containerapp logs show` for tailing.

## (c) Cost, monthly, EUR, West Europe

| item | price | monthly |
|---|---|---|
| Container App 0.5 vCPU / 1 GiB, always on (active rate 0.000024 USD/vCPU-s + 0.000003 USD/GiB-s, minus the free grant of 180k vCPU-s + 360k GiB-s) | | about 30 |
| Premium Files 100 GiB NFS share (0.169 EUR/GiB provisioned) | 0.169 | 17 |
| Log Analytics (ingestion, a few hundred MB) | | 1 to 3 |
| Key Vault, DNS records, managed certificate, environment, VNet | | 0 to 1 |
| Egress | first 100 GB free, then ~0.08/GB | ~0 |
| **total** | | **about 50** |

Fallback to cut the share: standard Files SMB (Hot LRS 0.0238 EUR/GB stored, a few GB = cents) saves about
16 EUR but is a CIFS mount through an account key (the key lands in the Terraform state, which the hub
identities can read), with no POSIX rename/lock guarantees for a maildir. For a board that is the team's
message bus, the 16 EUR buys the right semantics; I would not take the fallback.

## Blockers and gotchas (what I would test or decide before the go)

1. **WebSocket idle timeout.** Container Apps' HTTP ingress has a 240 s request timeout. Microsoft documents
   WebSocket support but no separate idle limit; in practice an idle WebSocket is cut around that mark. Plan:
   the server pings every 30 s (or clients do), the bridge reconnects with backoff and resumes from its
   cursor (conde already carries kond's delivery semantics, so a reconnect loses nothing). Verify on the
   first deployment with a 15-minute idle connection, not by reading docs.
2. **Single writer across rollouts.** min=max=1 does not stop Container Apps from running the new replica
   next to the old one for a few seconds during a revision switch (Single mode drains the old one after the
   new one is ready). Two writers on one maildir for seconds is survivable if writes are tmp+rename and ids
   are ULIDs (no overwrite), or the server takes an advisory lock on a lock file at startup and waits for it.
   Decide which; I would do the lock file.
3. **NFS semantics on Azure Files (NFS 4.1):** rename is atomic within the share, byte-range locks are
   advisory (POSIX lockf/fcntl and flock both map to NFSv4 locks), hard links work, fsync works, no
   mandatory locks, and file ownership is whatever uid the container runs as (no root squash by default on
   Azure Files NFS). Attribute caching: with one writer there is no cache coherence problem; mount options can
   add `actimeo=0` if the smoke test shows stale reads. The share root is created mode 0777 by Azure; the app
   should run as a fixed non-root uid and own its subdirectory.
4. **Smoke test on the real mount** (a Container Apps Job on the same environment, same NFS volume, image
   python:3.12-slim, one script, exit code = verdict), run before any data is written:
   - atomic rename: write `tmp/a` with content A, `os.rename` over an existing `new/a` with content B while a
     second thread reads `new/a` in a loop: every read returns exactly A or B, never empty or mixed;
   - durability: `os.fsync` on the file and on the directory succeed;
   - byte-range lock: process 1 takes `fcntl.lockf(fd, LOCK_EX)` on `lock`, process 2's non-blocking attempt
     gets EAGAIN/EACCES, blocks until release, then succeeds; same for `flock`;
   - hard link + unlink (maildir delivery), `os.link` count goes to 2 and back;
   - permissions: files created as the app's uid keep mode 0600 and are readable back after remount
     (second job run);
   - latency: 1000 small appends + fsync, report p50/p99 (premium NFS should sit well under 10 ms).
   I write that job as part of the Terraform root; it costs cents per run.
5. **Egress IP is not static** (no NAT gateway): irrelevant for conde (inbound service), stated so nobody
   plans an IP allowlist around it.
6. **Backups.** Azure Files share snapshots are not available on NFS shares via the backup service;
   plan a scheduled job that tars the board to a blob container (cheap, LRS) daily, or accept no backup for v1.
   Owner decision.
7. **Public exposure.** The ingress is on the internet; conde's own API-key auth is the only gate. Container
   Apps can restrict ingress by IP allowlist, but bridges run from home networks, so the key is the control.
   TLS 1.2+ enforced by the platform.
8. **Rust image.** `linux/amd64` only; a distroless or debian-slim image with the static binary is fine;
   the app must listen on 0.0.0.0 and the configured port.

## What I need from the owner (one line each)

- The hostname: conde.nativeai.space (automated) or conde.dakai.io (two Namecheap records by him).
- Backup for v1: daily tar to blob, or none.
- The go. From the go, the root plus the smoke-test job is about half a day; the first apply about 40 minutes
  (environment and Postgres-free, faster than meetings).
