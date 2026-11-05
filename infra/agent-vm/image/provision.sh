#!/usr/bin/env bash
# The image build's provisioner, as root on the temporary build VM (Packer uploads the files to /tmp/agentvm).
# What a VM made from the image has, before its first boot:
#   - everything bootstrap.sh installs, for the fixed user `agent` (linger, ~/work trusted, Claude state seeded,
#     the SessionStart hook in its settings), but no `conde setup`: that records the host, so it runs at first boot
#   - headless Chromium
#   - updates in place: unattended-upgrades for Ubuntu, the git PPA and gh (no automatic reboot: a reboot would kill
#     the agent's work in progress; a kernel update waits for a planned one), a daily `conde update`, Claude's own updater
#   - agentvm-firstboot.service, which joins the board with the VM's identity and starts the agent
set -euo pipefail
SRC=/tmp/agentvm
AGENT_USER=agent
export DEBIAN_FRONTEND=noninteractive

cloud-init status --wait >/dev/null || true   # its apt run holds the lock

install -d -m 0755 /usr/local/lib/agentvm
install -m 0755 "$SRC/bootstrap.sh" "$SRC/run-agent" "$SRC/conde-register-hook" "$SRC/agentvm-firstboot" \
  "$SRC/relay-claude" "$SRC/relay-github" "$SRC/agentvm-signin-check" /usr/local/lib/agentvm/
install -m 0644 "$SRC/claude-agent.service" "$SRC/agentvm-signin-check.service" "$SRC/agentvm-signin-check.timer" /etc/systemd/user/
install -m 0755 "$SRC/agentvm" /usr/local/bin/agentvm
# GITHUB_*: the relay's GitHub device flow. The pilot uses the gh CLI's public OAuth client id until the
# Dakai OAuth app exists (Krisztián, 5 Nov); switching is this one value.
cat > /etc/agentvm.conf <<CONF
AGENT_USER=$AGENT_USER
CONDE_SERVER_DEFAULT=https://dakai.conde.cloud
GITHUB_OAUTH_CLIENT_ID=178c6fc778ccc68e1d6a
GITHUB_OAUTH_SCOPES="repo read:org workflow"
CONF

bash /usr/local/lib/agentvm/bootstrap.sh image

# --- Chromium (Ubuntu ships it as a snap only; snaps refresh themselves)
snap install chromium

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
};
CONF

# --- conde updates itself daily (a user timer of the agent user; `conde update` restarts the bridge)
home=$(getent passwd "$AGENT_USER" | cut -d: -f6)
install -d -o "$AGENT_USER" -g "$AGENT_USER" "$home/.config/systemd/user/timers.target.wants"
cat > "$home/.config/systemd/user/conde-update.service" <<'UNIT'
[Unit]
Description=Update conde to the latest release

[Service]
Type=oneshot
ExecStart=%h/.local/bin/conde update
UNIT
cat > "$home/.config/systemd/user/conde-update.timer" <<'UNIT'
[Unit]
Description=Daily conde update

[Timer]
OnCalendar=daily
RandomizedDelaySec=2h
Persistent=true

[Install]
WantedBy=timers.target
UNIT
ln -sf ../conde-update.timer "$home/.config/systemd/user/timers.target.wants/conde-update.timer"
chown -R "$AGENT_USER:$AGENT_USER" "$home/.config/systemd"

# --- first boot
install -m 0644 "$SRC/agentvm-firstboot.service" /etc/systemd/system/
systemctl enable agentvm-firstboot.service

# --- nothing of this build VM may reach the VMs made from it
jq 'del(.userID)' "$home/.claude.json" > "$home/.claude.json.new" && mv "$home/.claude.json.new" "$home/.claude.json"
chown "$AGENT_USER:$AGENT_USER" "$home/.claude.json"
rm -rf "$home/.local/share/conde" "$home/.config/conde" /var/lib/agentvm/firstboot-done /var/lib/agentvm/bootstrap-done
rm -rf "$SRC"
cloud-init clean --logs
truncate -s 0 /etc/machine-id
rm -f /var/lib/dbus/machine-id
echo "== image provisioned $(date -u +%FT%TZ)"
