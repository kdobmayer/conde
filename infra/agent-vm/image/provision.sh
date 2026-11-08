#!/usr/bin/env bash
# The image build's provisioner, as root on the temporary build VM (Packer uploads the files to /tmp/agentvm).
# What a VM made from the image has, before its first boot:
#   - everything bootstrap.sh installs, for the fixed user `agent` (linger, ~/work trusted, Claude state seeded,
#     the SessionStart hook in its settings), but no `conde setup`: that records the host, so it runs at first boot
#   - headless Chrome
#   - updates in place: unattended-upgrades for Ubuntu, the git PPA and gh (no automatic reboot: a reboot would kill
#     the agent's work in progress; a kernel update waits for a planned one), a daily `conde update`, Claude's own updater
#   - agentvm-firstboot.service, which joins the board with the VM's identity and starts the agent
#   - praxia-connect (its service and retry timer, started by first boot): the agent's Praxia token, when the build
#     gives CONNECT_RESOURCE (Praxia's praxia-agent-connect application id); without it the VM never connects
set -euo pipefail
SRC=/tmp/agentvm
AGENT_USER=agent
export DEBIAN_FRONTEND=noninteractive

cloud-init status --wait >/dev/null || true   # its apt run holds the lock

install -d -m 0755 /usr/local/lib/agentvm
install -m 0755 "$SRC/bootstrap.sh" "$SRC/run-agent" "$SRC/conde-register-hook" "$SRC/agentvm-firstboot" \
  "$SRC/relay-claude" "$SRC/relay-github" "$SRC/relay-codex" "$SRC/agentvm-signin-check" "$SRC/praxia-connect" \
  /usr/local/lib/agentvm/
install -m 0644 "$SRC/claude-agent.service" "$SRC/agentvm-signin-check.service" "$SRC/agentvm-signin-check.timer" \
  "$SRC/praxia-connect.service" "$SRC/praxia-connect.timer" /etc/systemd/user/
install -m 0755 "$SRC/agentvm" /usr/local/bin/agentvm
# GITHUB_*: the relay's GitHub device flow. The pilot uses the gh CLI's public OAuth client id until the
# Dakai OAuth app exists (Krisztián, 5 Nov); switching is this one value.
cat > /etc/agentvm.conf <<CONF
AGENT_USER=$AGENT_USER
CONDE_SERVER_DEFAULT=https://dakai.conde.cloud
GITHUB_OAUTH_CLIENT_ID=178c6fc778ccc68e1d6a
GITHUB_OAUTH_SCOPES="repo read:org workflow"
PRAXIA_URL=https://praxia.nativeai.space
CONF
# The application id is not secret; the build passes it (agent-image.yml, the environment's PRAXIA_CONNECT_RESOURCE).
# First boot sources this file as root: only an application id's shape gets in.
if [ -n "${CONNECT_RESOURCE:-}" ]; then
  [[ $CONNECT_RESOURCE =~ ^[0-9a-f-]{36}$ ]] || { echo "CONNECT_RESOURCE is not an application id"; exit 1; }
  printf 'CONNECT_RESOURCE=%s\n' "$CONNECT_RESOURCE" >> /etc/agentvm.conf
else
  echo "warning: no CONNECT_RESOURCE: the VMs of this image never connect to Praxia"
fi

bash /usr/local/lib/agentvm/bootstrap.sh image

# --- Chrome, headless (Google's apt repository: apt-fast and updated in place; the Chromium snap took 37 minutes in
# one build). The package writes its own source (/etc/apt/sources.list.d/google-chrome.sources, Signed-By its own
# keyring) at install, so the source here is temporary: fingerprint-checked, used once, removed.
GOOGLE_KEYS="EB4C1BFD4F042F6DDDCCEC917721F63BD38B4796"
gk=$(mktemp -d)
curl -fsSL https://dl.google.com/linux/linux_signing_key.pub -o "$gk/key.pub"
got=$(gpg --show-keys --with-colons "$gk/key.pub" 2>/dev/null \
      | awk -F: '$1 == "pub" {want = 1; next} $1 == "fpr" && want {print $10; want = 0}' | sort | xargs)
[ "$got" = "$GOOGLE_KEYS" ] || { echo "Google apt key: unexpected primary keys: $got"; exit 1; }
gpg --dearmor < "$gk/key.pub" > "$gk/google.gpg" && chmod 0644 "$gk/google.gpg" && chmod 0755 "$gk"
echo "deb [arch=amd64 signed-by=$gk/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
  > /etc/apt/sources.list.d/google-chrome-bootstrap.list
apt-get update
apt-get -y install google-chrome-stable
rm -f /etc/apt/sources.list.d/google-chrome-bootstrap.list; rm -rf "$gk"
apt-get update
# Captured, not piped into grep -q: under pipefail, grep stopping early would fail Chrome with SIGPIPE.
dom=$(timeout 60 sudo -u "$AGENT_USER" -H google-chrome --headless --disable-gpu --dump-dom 'data:text/html,<p>agentvm-chrome-ok</p>' 2>/dev/null || true)
grep -q agentvm-chrome-ok <<<"$dom" || { echo "Chrome smoke check failed (as $AGENT_USER, with its sandbox)"; exit 1; }

# --- Ubuntu updates in place, plus the two third-party repos the bootstrap added
apt-get -y install unattended-upgrades
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'CONF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
CONF
cat > /etc/apt/apt.conf.d/52agentvm-upgrades <<'CONF'
Unattended-Upgrade::Origins-Pattern {
  "origin=LP-PPA-git-core";
  "site=cli.github.com";
  "origin=Google LLC";
};
CONF

# --- conde updates itself daily: a user timer, root-owned in /etc/systemd/user and enabled for the agent by first
# boot (nothing root writes goes into the agent's folders); `conde update` runs as the agent and restarts the bridge.
home=$(getent passwd "$AGENT_USER" | cut -d: -f6)
cat > /etc/systemd/user/conde-update.service <<'UNIT'
[Unit]
Description=Update conde to the latest release

[Service]
Type=oneshot
ExecStart=%h/.local/bin/conde update
UNIT
cat > /etc/systemd/user/conde-update.timer <<'UNIT'
[Unit]
Description=Daily conde update

[Timer]
OnCalendar=daily
RandomizedDelaySec=2h
Persistent=true

[Install]
WantedBy=timers.target
UNIT
chmod 0644 /etc/systemd/user/conde-update.service /etc/systemd/user/conde-update.timer

# --- first boot
install -m 0644 "$SRC/agentvm-firstboot.service" /etc/systemd/system/
systemctl enable agentvm-firstboot.service

# --- nothing of this build VM may reach the VMs made from it
sudo -u "$AGENT_USER" -H bash -c 'jq "del(.userID)" ~/.claude.json > ~/.claude.json.new && mv ~/.claude.json.new ~/.claude.json'
rm -rf "$home/.local/share/conde" "$home/.config/conde" /var/lib/agentvm/firstboot-done /var/lib/agentvm/bootstrap-done
rm -rf "$SRC"
cloud-init clean --logs
truncate -s 0 /etc/machine-id
rm -f /var/lib/dbus/machine-id
echo "== image provisioned $(date -u +%FT%TZ)"
