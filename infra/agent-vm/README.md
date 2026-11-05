# Agent VMs

One VM per person, so that person's conde agent stays reachable when their own computer is off. The VM runs the
conde bridge and one interactive Claude Code session with remote control on. The person uses it from claude.ai/code
or the Claude app; power users also SSH in over the hub VPN and attach to its tmux. Teammates' agents reach it on
conde.

The design is Krisztián's always-on agent design of 4–5 Nov 2026 (settled with Konstantin's side). This folder is
the Azure half and the VM half. The conde server half (trigger, machine records, the mint endpoint, the onboarding
page and the relay) is in conde itself.

## Two ways a VM is made

| | image VMs (the design) | test flow (manual, before the image) |
|---|---|---|
| made by | the conde server deploys `template/agent-vm.bicep` (template spec `agent-vm`) | `./agentvm.sh create <person>` |
| software | pre-baked image `galNativeLanes/agent-vm`, updated in place | `bootstrap.sh` at first boot (cloud-init) |
| user | fixed `agent` | the person's short name |
| joins the board | the VM's own identity: the mint call at first boot (`agentvm-firstboot`) | `agentvm conde-login-*`, the person signs in |
| Claude sign-in | relayed by the onboarding page (bridge) | `agentvm claude-login-*` through `az vm run-command` |
| access | Entra SSH over the hub VPN | none: only the remote-controlled session |

## Files

| file | runs | does |
|---|---|---|
| `bootstrap.sh` | root; Packer (`image`) or cloud-init | installs from current releases: Ubuntu updates, git (git-core PPA), gh, Claude Code, uv, conde; the agent user with linger; `~/work` trusted, onboarding and bypass-mode prompt pre-answered, the SessionStart hook. Without `image` it also runs `conde setup` |
| `image/agent-vm.pkr.hcl` | Packer | the image: Ubuntu 24.04 server, `bootstrap.sh image` + `image/provision.sh`, generalized, into `galNativeLanes/agent-vm` |
| `image/provision.sh` | Packer, root | Chromium (snap), unattended-upgrades for Ubuntu, the git PPA and gh (no automatic reboot), a daily `conde update` user timer, `agentvm-firstboot.service`, clean-up of this build machine's identity |
| `agentvm-firstboot` (+ `.service`) | image VM, first boot, root | identity token from the metadata service; `POST /v1/machines/enrol`; `conde enroll --key-stdin` (retried while the key is held); then `conde setup`; agent.env with a pinned session id; `claude-agent.service` enabled |
| `claude-agent.service` | root-owned in `/etc/systemd/user`, enabled for the agent user | tmux around the root-owned `/usr/local/lib/agentvm/run-agent`; `Restart=always` |
| `run-agent` | agent user, in tmux | waits for the Claude sign-in; first start `--session-id <uuid>`, every later start `--resume <uuid>` with a recovery prompt; never `--continue` |
| `conde-register-hook` | Claude SessionStart hook | registers the conde name for the agent's own session id only (a power user's own `claude` runs the hook too and is skipped) |
| `agentvm` | VM, root | the test flow's steps, and `status`, `agent-log` |
| `agentvm.sh` | operator, locally | the test flow: `create`, `run`, `delete` |
| `pilot-rebuild.sh` | operator, locally, pilot only | the rebuild's steps that are not one command: `inventory`, the manual `join`, `signin` by hand, `publish`; deleted with the enrol route |
| `template/agent-vm.bicep` | conde server (template spec) | one VM: NIC, Standard SSD OS disk, system-assigned identity, Entra SSH extension, Virtual Machine Administrator Login for the person, userData (conde server and token resource). A fixed admin SSH public key whose private half was destroyed. Parameters: owner, ownerObjectId, condeResource (required: the conde-machines application id), vmSize (B2as_v2 by default, D2as_v5 for the pilot). Output `principalId` = the `oid` the mint call checks |
| `azure/gallery.bicep` | once, RG-NATIVE-LANES | image definition `agent-vm` (generalized, Gen2); the image workflow writes versions, id-conde reads them |
| `azure/agent-rg.bicep` | once, conde-vm | id-conde: the custom role Agent VM Deployer (VMs, extensions, NICs, OS disks, subnet join, deployments, template spec read; no run-command, no disk export, no network writes) and RBAC Administrator conditioned to Virtual Machine Administrator Login for users; Azure Policy: only the AADSSHLoginForLinux extension (Deny), since RBAC cannot narrow extension writes. Accepted residual: the identity could create its own VM in the subnet with a copy of a person's OS disk |
| `azure/agent-network.bicep` | once, conde-vm | NSG: SSH from the VPN pool 172.16.0.0/22, every other inbound from the VNet denied, every outbound into the VNet denied; the agent side of the hub peering (remote gateways), after the rules. DNS stays Azure's default |
| `azure/hub-peer-access.bicep` | once, conde-vm | custom role Agent VNet Peer (read and peer on vnet-agent-vms) for native-infra's hub deployer, so its Terraform can create the hub side |
| `azure/image-builder.sh` | once, by hand | Entra app `conde-agent-image` (GitHub OIDC, environment `agent-image` of kdobmayer/conde), build resource group `rg-agent-image`; with `GITHUB=1`, as the repo admin, the environment with main as its only branch policy and its variables, asserted at the end |
| `../../.github/workflows/agent-image.yml` | GitHub-hosted runner | `packer build`, version `1.<yyyymmdd>.<run>` |

## Setting up the Azure half (in order; each is a state change outside dev, approved by Konstantin first)

```sh
infra/agent-vm/azure/image-builder.sh                                     # prints builderPrincipalId and the GitHub side
az deployment group create -g RG-NATIVE-LANES -f infra/agent-vm/azure/gallery.bicep -p builderPrincipalId=<id>
az deployment group create -g conde-vm -f infra/agent-vm/azure/hub-peer-access.bicep
#   ... native-infra applies the hub side (terraform/hub/network.tf), then:
az deployment group create -g conde-vm -f infra/agent-vm/azure/agent-network.bicep
az deployment group create -g conde-vm -f infra/agent-vm/azure/agent-rg.bicep          # roles and the extension policy
gh workflow run agent-image.yml --repo kdobmayer/conde                    # the first image version, from main
az ts create -g conde-vm -n agent-vm -v 1 -f infra/agent-vm/template/agent-vm.bicep
```

The hub side of the peering is native-infra's Terraform (`terraform/hub/network.tf`:
`peer-hub-to-vnet-agent-vms`, gateway transit). Windows VPN users download their VPN client profile again once
after the peering. The policy permits only AADSSHLoginForLinux extension resources. The custom role omits both
run-command permissions; the policy itself does not block operators who hold them. The policy matches the extension's
type, not its publisher (an accepted residual).

Until the Entra app conde-machines exists (Krisztián's half), the deployment caller passes the token audience:
`-p condeResource=<conde-machines application id>`. The template has no default.

## What a person's VM does, start to finish

1. The server deploys the template spec as `agent-vm-<owner>` with the person's name, Entra object id and size, and
   records the VM identity's `principalId` in the machine record as soon as the VM resource exists (not from the
   deployment outputs, which come only after the extension).
2. First boot (`agentvm-firstboot`): a token for the VM's identity with conde-machines as the resource,
   `POST /v1/machines/enrol` (network errors, 404, 5xx, and 429 with Retry-After retried for 30 minutes; every other
   4xx is final), `conde enroll` with the key on stdin, then `conde setup`. The key is never in an argument, a file
   of ours or the log.
3. `claude-agent.service` starts and waits for the Claude sign-in (the page relays it).
4. Once signed in, Claude starts under the pinned session id with remote control on; the SessionStart hook
   registers the name; the server publishes it when the registration arrives from that host.
5. Every later start (reboot, crash, a new sign-in) resumes the same session with a short recovery prompt: finish
   interrupted work without repeating completed external actions, then read the conde inbox. Recovery assistance,
   not completion tracking.

## Sign-in expiry (pilot)

`agentvm-signin-check` prints `claude ok|expiring|expired|unknown <loginExpiresAt>` and `github ok|expired`. Its
hourly user timer runs it with `--notify`, which pages the human through conde (`conde page`, as the agent's own
session) when the Claude sign-in has under three days left or has ended: once a day per condition, retried next
hour if the page failed or was stored without reaching Slack (the board needs the person's Slack id). When the
bridge sends `machine_event`, it runs the same script and the timer goes.

## Rebuilding a person's VM (pilot: Levente, Oliver)

Moves a person from a test-flow VM (`vm-agent-<short>`, user = their short name) onto an image VM
(`vm-agent-<owner>`, user `agent`). The names differ, so the new VM is staged first. What cannot carry over: the
old agent's conversation history, and files on the old VM nobody copied out. Each old VM is deleted only on
Krisztián's go, per VM. Steps 2 and 4 are `pilot-rebuild.sh` until the enrol route and the onboarding page exist;
then the server does them and the script is deleted.

1. **Inventory**, and the person copies out what they keep: `./pilot-rebuild.sh inventory <short>`.
2. **Stage the new VM** (the server's "set up my agent", or by hand):

   ```sh
   az deployment group create -g conde-vm -n agent-vm-<owner> \
     --template-spec "$(az ts show -g conde-vm -n agent-vm -v <version> --query id -o tsv)" \
     -p owner=<owner> ownerObjectId=<Entra object id> condeResource=<conde-machines app id> vmSize=Standard_D2as_v5
   ```

   Until the enrol route exists: `./pilot-rebuild.sh join <owner>` (the person signs in to conde),
   `./pilot-rebuild.sh join <owner> CODE`.
3. **Sign-ins** through the page, or `./pilot-rebuild.sh signin <owner> claude` and `signin <owner> code CODE`,
   then `signin <owner> github`. The hook registers the name locally.
4. **Move the name**: on the old VM, as its user, `conde unpublish vm.agent --board dakai`; then
   `./pilot-rebuild.sh publish <owner>` (the server publishes it itself once the enrol route exists).
5. **Check with a real request**: another host's agent sends `<owner>/vm.agent` a question on dakai and gets the
   answer.
6. **Retire the old VM**, on Krisztián's go: stop its agent (`systemctl --user disable --now claude-agent` as its
   user); ask desk to revoke the old host's keys (`vm-agent-<short>`; the admin API, which also disconnects its
   bridge); check its delete settings
   (`az vm show -g conde-vm -n vm-agent-<short> --query "[networkProfile.networkInterfaces[0].deleteOption, storageProfile.osDisk.deleteOption]"`:
   the test flow's VMs keep NIC and disk on delete); then `AGENTVM_RG=conde-vm ./agentvm.sh delete <short>`, which
   deletes VM, NIC and disk.

After both pilot VMs: `agent-rg.bicep` (the server's roles and the extension policy).

## Existing VMs (made before 5 Nov)

They carry a copy of the agent unit in `~/.config/systemd/user`, which takes precedence over the root-owned
`/etc/systemd/user/claude-agent.service`. With the new files in place (`/etc/systemd/user/claude-agent.service`,
`/usr/local/lib/agentvm/run-agent`), as the agent user:

```sh
rm -f ~/.config/systemd/user/claude-agent.service ~/.config/agentvm/run-agent
systemctl --user daemon-reload && systemctl --user enable --now claude-agent && systemctl --user restart claude-agent
```

## Watching and steering the agent (power users)

Over the hub VPN, with your Entra account (`az ssh vm -g conde-vm -n vm-agent-<owner>`), then:

```sh
sudo -u agent tmux -L agent attach -t agent      # the agent's own session; detach with Ctrl-b d
journalctl -u agentvm-firstboot                  # how the VM joined the board
```

## Test flow (manual, `vm-conde-test`)

```sh
./agentvm.sh create <person>                         # ~5-10 min of cloud-init
./agentvm.sh run <person> status                     # wait for "bootstrap: done"
./agentvm.sh run <person> claude-login-start         # the Claude sign-in URL for the person
./agentvm.sh run <person> claude-login-finish CODE   # the code the page showed them
./agentvm.sh run <person> conde-login-start          # the conde (dakai) sign-in URL
./agentvm.sh run <person> conde-login-finish CODE    # publishes nothing
./agentvm.sh run <person> agent-start <conde name>   # remote control session; the hook registers the name
```

`AGENTVM_RG` and `AGENTVM_SIZE` override the resource group (`vm-conde-test`) and size (`Standard_B2s`).

## The relay helpers

The bridge (conde's code) handles the relay frames and the machine events, and execs one of two helpers per
relay, as the agent user, with a line protocol on stdin and stdout:

| helper | in | out |
|---|---|---|
| `relay-claude` | `start`, then `code <code>`; EOF or SIGTERM cancels | `url <authorize URL>`, `waiting`, `done`, `failed <reason>` |
| `relay-github` | `start`; EOF or SIGTERM cancels | `url <verification URL> <user_code> <expires_at>`, `waiting`, `done`, `failed <reason>` |

Reasons are the contract's: `expired`, `denied`, `bad_code`, `process_exited`, `timeout`, `busy`. Until the bridge
side exists the pilot runs them by hand, keeping stdin open (closing it cancels):
`{ echo start; sleep 900; } | relay-github`.

## Not here yet

- The bridge's handling of the relay frames and the machine events (`online`, `needs_sign_in`): conde's half.
- The rebuild of Levente's and Oliver's VMs (above), after the probe passed on 5 Nov.
