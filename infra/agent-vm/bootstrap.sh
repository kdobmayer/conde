#!/usr/bin/env bash
# Installs an agent VM (Ubuntu 24.04), as root, from the vendors' current releases.
#
#   bootstrap.sh          the test flow: cloud-init runs it at first boot (install, then `conde setup`)
#   bootstrap.sh image    the Packer build: install only. `conde setup` is per machine (it records this
#                         host), so on image VMs it runs at first boot, in agentvm-firstboot
#
# Nothing here signs in: the sign-ins need the person and come later.
set -euo pipefail
MODE="${1:-full}"

[ -f /etc/agentvm.conf ] && . /etc/agentvm.conf
AGENT_USER="${AGENT_USER:-agent}"
WORK_DIR_NAME="${WORK_DIR_NAME:-work}"
DONE=/var/lib/agentvm/bootstrap-done

exec > >(tee -a /var/log/agentvm-bootstrap.log) 2>&1
echo "== bootstrap start $(date -u +%FT%TZ)"
export DEBIAN_FRONTEND=noninteractive

# --- System packages: current Ubuntu updates, git from the git-core PPA (Ubuntu's lags), gh from GitHub's repo.
apt-get update
apt-get -y -o Dpkg::Options::=--force-confnew upgrade
apt-get -y install ca-certificates curl gnupg software-properties-common \
  tmux jq ripgrep unzip build-essential python3 python3-venv uuid-runtime
add-apt-repository -y ppa:git-core/ppa
install -d -m 0755 /etc/apt/keyrings
# The keyring holds two primary keys: 2C61...6059 (expired 2026-09-05) and 7F38...3325, which signs the current
# InRelease. The whole primary-key set must be exactly these two, or the bootstrap stops.
GH_KEYS="2C6106201985B60E6C7AC87323F3D4EA75716059 7F38BBB59D064DBCB3D84D725612B36462313325"
curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /tmp/githubcli-archive-keyring.gpg
got=$(gpg --show-keys --with-colons /tmp/githubcli-archive-keyring.gpg 2>/dev/null \
      | awk -F: '$1 == "pub" {want = 1; next} $1 == "fpr" && want {print $10; want = 0}' | sort | xargs)
[ "$got" = "$(tr ' ' '\n' <<<"$GH_KEYS" | sort | xargs)" ] || { echo "gh apt keyring: unexpected primary keys: $got"; exit 1; }
install -m 0644 /tmp/githubcli-archive-keyring.gpg /etc/apt/keyrings/githubcli-archive-keyring.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
  > /etc/apt/sources.list.d/github-cli.list
apt-get update
apt-get -y install git gh

# --- The agent's own user. Linger starts its systemd user manager at boot, so the conde bridge and the
# agent run without anyone logged in.
id "$AGENT_USER" >/dev/null 2>&1 || useradd -m -s /bin/bash "$AGENT_USER"
loginctl enable-linger "$AGENT_USER"
uid=$(id -u "$AGENT_USER")
for _ in $(seq 60); do [ -S "/run/user/$uid/bus" ] && break; sleep 1; done
[ -S "/run/user/$uid/bus" ] || { echo "user manager of $AGENT_USER did not start"; exit 1; }

as_agent() {
  sudo -u "$AGENT_USER" -H env XDG_RUNTIME_DIR="/run/user/$uid" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" PATH="/home/$AGENT_USER/.local/bin:/usr/local/bin:/usr/bin:/bin" \
    bash -lc "set -euo pipefail; cd ~ && $1"
}

# --- User-level tools, each from its own installer (latest release; the conde installer checks SHA256SUMS). Each
# installer is downloaded first and runs only if the download succeeded; a failed one stops the bootstrap.
install_from() {  # $1 URL, $2 shell
  as_agent "d=\$(mktemp -d); trap 'rm -rf \"\$d\"' EXIT; curl -fsSL $(printf '%q' "$1") -o \"\$d/install.sh\" && $2 \"\$d/install.sh\""
}
install_from https://claude.ai/install.sh bash
install_from https://astral.sh/uv/install.sh sh
install_from https://conde.cloud/install.sh bash
verify_tools() { as_agent 'echo "claude $(claude --version)"; conde --version; uv --version'; git --version; gh --version | head -1; }

# --- Claude Code state that a terminal would otherwise ask for: onboarding done, the work folder
# trusted (remote control asks to trust a folder, and there is no terminal to answer), and bypass mode
# accepted for this dedicated VM.
as_agent "mkdir -p ~/$WORK_DIR_NAME ~/.claude && cd ~/$WORK_DIR_NAME && git init -q 2>/dev/null || true"
as_agent "f=~/.claude.json; [ -f \$f ] || echo '{}' > \$f; \
  jq --arg d \"\$HOME/$WORK_DIR_NAME\" '.hasCompletedOnboarding = true
     | .projects[\$d].hasTrustDialogAccepted = true
     | .projects[\$d].hasCompletedProjectOnboarding = true' \$f > \$f.new && mv \$f.new \$f"
as_agent "f=~/.claude/settings.json; [ -f \$f ] || echo '{}' > \$f; \
  jq '.skipDangerousModePermissionPrompt = true
     | .hooks.SessionStart = [{hooks: [{type: \"command\", command: \"/usr/local/lib/agentvm/conde-register-hook\"}]}]' \
  \$f > \$f.new && mv \$f.new \$f"

# --- conde: the bridge as a user service, the skill and MCP entry for Claude Code, receiver policy accept.
# Joining the shared board (dakai) comes later: `agentvm conde-login-*` (test flow) or the mint call at first
# boot (image VMs). The image build stops before this, because `conde setup` records the host.
if [ "$MODE" = image ]; then
  verify_tools
  echo "== bootstrap (image) done $(date -u +%FT%TZ)"
  exit 0
fi
as_agent 'conde setup'
as_agent 'conde setup --check' || true

verify_tools
mkdir -p "$(dirname "$DONE")"
date -u +%FT%TZ > "$DONE"
echo "== bootstrap done $(date -u +%FT%TZ)"
