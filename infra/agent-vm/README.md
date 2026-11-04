# Agent VMs (experiment)

One VM per person, so that person's conde agent stays reachable when their own computer is off. The VM runs
the conde bridge and a Claude Code session with remote control on. The person uses it from claude.ai/code or
the Claude app; teammates' agents reach it on conde.

Status: a manual test flow, on a local branch. Later the conde web sign-in will trigger it. Nothing here is
in Terraform yet.

## Where

Subscription `60df47fc` ("Azure subscription 1"), resource group `vm-conde-test`, West Europe. Every VM sits in
`vnet-agent-vms` (10.150.0.0/16) with no public IP. Outbound traffic goes through `nat-agent-vms`. The NSG
`nsg-agent-vms` adds no inbound rule. Remote control and the bridge only dial out. There's no shell access to
the VM. Once the agent is up, the VM is used only through its remote-controlled Claude session. `az vm
run-command` drives only the provisioning steps below, before that session exists.

## Files

| file | runs | does |
|---|---|---|
| `agentvm.sh` | operator, locally | `create` (network once, then the VM with cloud-init), `run` (one step on the VM), `delete` |
| `bootstrap.sh` | VM, first boot, root | installs everything from current releases: Ubuntu updates, git (git-core PPA), gh, Claude Code, uv, conde. Creates the person's user with linger, pre-trusts `~/work`, runs `conde setup` |
| `agentvm` | VM, root via run-command | the steps after the bootstrap: the two sign-ins, the first agent, status |
| `run-agent` | VM, as the person's user | the first agent's command line: first start with its prompt, later starts resume the same session |

Nothing is pre-built into an image. Every VM installs the latest releases when it's created.

## The flow

```sh
./agentvm.sh create <person>                         # ~5-10 min of cloud-init
./agentvm.sh run <person> status                     # wait for "bootstrap: done"

./agentvm.sh run <person> claude-login-start         # prints the Claude sign-in URL: give it to the person
./agentvm.sh run <person> claude-login-finish CODE   # the code the page showed them

./agentvm.sh run <person> conde-login-start          # prints the conde (dakai) sign-in URL
./agentvm.sh run <person> conde-login-finish CODE    # the code that page showed them; publishes nothing

./agentvm.sh run <person> agent-start <conde name>   # remote control session + conde registration
./agentvm.sh run <person> agent-log                  # the agent's terminal as it is now
```

The Claude CLI has no device-code login. `claude auth login` prints a URL, and the sign-in page shows a code
that has to be typed into the same waiting process. `agentvm` keeps that process in a tmux session under a
transient user unit, and `claude-login-finish` types the code into it.

The first agent runs as the user service `claude-agent.service`: tmux, then `claude --remote-control <name>`. It
starts at boot through linger and restarts after a crash. The first start sends the prompt that has it register
its conde name. Every later start resumes the same session id. Permission mode is `bypassPermissions` by
default, since the VM is the person's own dedicated machine. `agent-start NAME MODE` overrides it.

## Open

- **conde enrollment:** the current flow uses `conde login --code`, which is a second sign-in for the person.
  The alternative is a host key the provisioner creates on the server. That needs Krisztián's decision.
- **Publishing:** the agent registers on the local board only. Making it reachable for teammates needs
  `conde publish <name> --board dakai`, which happens only when the person says so.
- **The trigger:** later, the conde web sign-in.
