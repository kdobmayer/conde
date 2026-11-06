#!/usr/bin/env bash
# PILOT ONLY: delete this script when the relay and the publication exist, and its `join` after the two pilot joins.
# The runbook is README.md, "Rebuilding a person's VM"; this script holds only its steps that are not one command:
#
#   ./pilot-rebuild.sh inventory OLD           read-only: what would be lost on vm-agent-OLD (unpushed commits,
#                                              uncommitted files, ~/work's newest files, the gh login)
#   ./pilot-rebuild.sh join OWNER              pilot migration only (Levente, Oliver; remove after both joins):
#                                              conde's own sign-in for the person on vm-agent-OWNER (prints the URL)
#   ./pilot-rebuild.sh join OWNER CODE         finishes it, then first boot without the mint (marker, machine.env)
#   ./pilot-rebuild.sh signin OWNER claude|github|codex   no page yet: the relay helper by hand, input kept open (a FIFO)
#   ./pilot-rebuild.sh signin OWNER code CODE  the Claude code the person got; prints the relay's outcome
#   ./pilot-rebuild.sh publish OWNER           publishes vm.agent on dakai (once the old host has unpublished it)
#
# Env: AGENTVM_SUBSCRIPTION, AGENTVM_RG (default conde-vm).
set -euo pipefail

SUB="${AGENTVM_SUBSCRIPTION:-60df47fc-ce8f-4f52-a970-43a412d78007}"
RG="${AGENTVM_RG:-conde-vm}"
az() { command az "$@" --subscription "$SUB"; }

name_ok() { [[ $1 =~ ^[a-z][a-z0-9-]{0,40}$ ]] || { echo "bad name: $1" >&2; exit 64; }; }

# One bash script on the VM as root (run-command; works until the operators lose run-command). run-command
# runs /bin/sh, so the script travels base64 to bash; its stderr is shown after its stdout.
on_vm() {  # $1 VM name, $2 script
  local b64; b64=$(printf '%s' "$2" | base64 -w0)
  az vm run-command invoke -g "$RG" -n "$1" --command-id RunShellScript --scripts "echo $b64 | base64 -d | bash" \
    --query 'value[0].message' -o tsv | sed '/^Enable succeeded/d; /^\[stdout\]$/d; s/^\[stderr\]$/-- stderr:/' | sed '/^-- stderr:$/{N;/\n$/d}'
}

# Runs as the VM's agent user: the old VMs' user is the short name (OLD), the image VMs' is `agent`.
as_user() {  # $1 user, $2 commands
  printf 'u=%q; uid=$(id -u "$u"); sudo -u "$u" -H env XDG_RUNTIME_DIR=/run/user/$uid PATH=/home/$u/.local/bin:/usr/local/bin:/usr/bin:/bin bash -lc %q' "$1" "cd ~ && $2"
}

cmd_inventory() {
  local old="${1:?usage: rebuild.sh inventory OLD}"; name_ok "$old"
  on_vm "vm-agent-$old" "$(as_user "$old" '
echo "== git repositories under ~ (unpushed commits, uncommitted changes)"
find ~ -maxdepth 4 -name .git -type d -prune 2>/dev/null | while read -r g; do
  r=${g%/.git}; cd "$r" || continue
  ahead=$(git log --oneline --branches --not --remotes 2>/dev/null | wc -l)
  dirty=$(git status --porcelain 2>/dev/null | wc -l)
  remote=$(git remote get-url origin 2>/dev/null || echo "(no remote)")
  echo "  $r: $ahead unpushed commits, $dirty uncommitted files, origin $remote"
done
echo "== ~/work, newest 15 files outside .git"
find ~/work -path "*/.git" -prune -o -type f -printf "%TY-%Tm-%Td %8s %p\n" 2>/dev/null | sort -r | head -15
echo "== size"; du -sh ~/work ~/.claude 2>/dev/null
echo "== agent"; systemctl --user is-active claude-agent; . ~/.config/agentvm/agent.env 2>/dev/null && echo "session $SESSION_ID ($AGENT_NAME)"
echo "== conde"; conde boards | tail -n +2
echo "== gh"; gh auth status 2>&1 | grep -E "Logged in|not logged" | head -2')"
}





# The manual join, the pilot migration's path (README, "Rebuilding a person's VM"; remove after Levente's and Oliver's
# joins): conde login as the person, then first boot without the mint. First boot needs the enrolment marker and
# machine.env supplied; conde login publishes nothing (publish does, later).
cmd_join() {
  local owner="${1:?usage: pilot-rebuild.sh join OWNER [CODE]}" code="${2:-}"; name_ok "$owner"
  if [ -z "$code" ]; then
    on_vm "vm-agent-$owner" "systemctl stop agentvm-firstboot 2>/dev/null; $(as_user agent 'conde login --server https://dakai.conde.cloud --code 2>&1 | grep -E "https://" | head -1')"
    return
  fi
  [[ $code =~ ^[A-Za-z0-9-]{8,9}$ ]] || { echo "the conde code looks like ABCD-1234" >&2; exit 64; }   # conde validates it
  on_vm "vm-agent-$owner" "$(as_user agent "conde login --server https://dakai.conde.cloud --code $code 2>&1 | tail -n 1; conde boards | tail -n +2")
printf '%s=%q\\n' OWNER $(printf '%q' "$owner") FULL_NAME $(printf '%q' "$owner/vm.agent") BOARD dakai > /var/lib/agentvm/machine.env
date -u +%FT%TZ > /var/lib/agentvm/enrolled
systemctl start agentvm-firstboot; journalctl -u agentvm-firstboot --no-pager -o cat | grep agentvm-firstboot | tail -2"
}

# The relay by hand (no onboarding page): the helper reads a FIFO kept open, so it waits across separate
# run-commands (EOF would cancel it).
cmd_signin() {
  local owner="${1:?usage: pilot-rebuild.sh signin OWNER claude|github|codex|code CODE}" what="${2:?}"; name_ok "$owner"
  case "$what" in
    claude|github|codex)
      on_vm "vm-agent-$owner" "$(as_user agent "d=\$HOME/.relay-$what; systemctl --user stop relay-$what-byhand 2>/dev/null; rm -rf \$d; mkdir -p \$d; mkfifo \$d/in
systemd-run --user --quiet --unit=relay-$what-byhand bash -c \"exec 3<>\$d/in; /usr/local/lib/agentvm/relay-$what <&3 >\$d/out 2>\$d/err\"
timeout 10 bash -c 'echo start > \"\$1\"' _ \$d/in || { echo 'relay did not start'; exit 1; }
for i in \$(seq 40); do grep -q '^url ' \$d/out 2>/dev/null && break; sleep 1; done; cat \$d/out \$d/err
if [ $what = claude ]; then echo \"the code is accepted until \$(date -u -d '+10 min' +%H:%M) UTC (10 minutes); after that run signin $owner claude again\"; fi")" ;;
    code)
      local code="${3:?usage: pilot-rebuild.sh signin OWNER code CODE}"
      [[ $code =~ ^[A-Za-z0-9_#-]+$ ]] || { echo "unexpected characters in the code" >&2; exit 64; }
      # The relay gives up 10 minutes after its URL; a write into the FIFO with no reader would block for good (and
      # every later run-command with it), so check the relay first and bound the write.
      on_vm "vm-agent-$owner" "$(as_user agent "d=\$HOME/.relay-claude
if ! systemctl --user is-active --quiet relay-claude-byhand; then
  echo 'relay gone (its 10-minute code window passed?): run signin $owner claude again'; grep -hv '^url ' \$d/out \$d/err 2>/dev/null; exit 1
fi
timeout 10 bash -c 'printf \"code %s\\n\" \"\$2\" > \"\$1\"' _ \$d/in '$code' || { echo 'relay not reading: run signin $owner claude again'; exit 1; }
for i in \$(seq 90); do grep -qE '^(done|failed)' \$d/out && break; sleep 1; done; grep -v '^url ' \$d/out; cat \$d/err")" ;;
    *) exit 64 ;;
  esac
}

# Publishing is the session hook's once the image carries it; until then by hand, after the old host unpublished.
cmd_publish() {
  local owner="${1:?usage: pilot-rebuild.sh publish OWNER}"; name_ok "$owner"
  on_vm "vm-agent-$owner" "$(as_user agent 'conde publish vm.agent --board dakai 2>&1 | head -2; conde boards | tail -n +2')"
}

case "${1:-}" in
  inventory) shift; cmd_inventory "$@" ;;
  join) shift; cmd_join "$@" ;;
  signin) shift; cmd_signin "$@" ;;
  publish) shift; cmd_publish "$@" ;;
  *) sed -n '2,14p' "$0"; exit 64 ;;
esac
