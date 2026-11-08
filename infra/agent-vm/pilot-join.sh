#!/usr/bin/env bash
# PILOT MIGRATION ONLY: delete this script once Levente's and Oliver's replacement VMs are joined.
# The manual join of a replacement image VM (README.md, "Rebuilding a person's VM", step 2): conde's own sign-in
# as the person on vm-agent-OWNER, then first boot without the mint (the enrolment marker and machine.env supplied).
#
#   ./pilot-join.sh OWNER          prints the conde sign-in URL for the person
#   ./pilot-join.sh OWNER CODE     finishes it with the code the person got, then runs first boot
#
# Env: AGENTVM_SUBSCRIPTION, AGENTVM_RG (default conde-vm).
set -euo pipefail

SUB="${AGENTVM_SUBSCRIPTION:-60df47fc-ce8f-4f52-a970-43a412d78007}"
RG="${AGENTVM_RG:-conde-vm}"
az() { command az "$@" --subscription "$SUB"; }

name_ok() { [[ $1 =~ ^[a-z][a-z0-9-]{0,40}$ ]] || { echo "bad name: $1" >&2; exit 64; }; }

# One bash script on the VM as root (run-command runs /bin/sh, so the script travels base64 to bash); its stderr is
# shown after its stdout.
on_vm() {  # $1 VM name, $2 script
  local b64; b64=$(printf '%s' "$2" | base64 -w0)
  az vm run-command invoke -g "$RG" -n "$1" --command-id RunShellScript --scripts "echo $b64 | base64 -d | bash" \
    --query 'value[0].message' -o tsv | sed '/^Enable succeeded/d; /^\[stdout\]$/d; s/^\[stderr\]$/-- stderr:/' | sed '/^-- stderr:$/{N;/\n$/d}'
}

# Runs as the image VM's agent user.
as_agent() {  # $1 commands
  printf 'u=agent; uid=$(id -u "$u"); sudo -u "$u" -H env XDG_RUNTIME_DIR=/run/user/$uid PATH=/home/$u/.local/bin:/usr/local/bin:/usr/bin:/bin bash -lc %q' "cd ~ && $1"
}

owner="${1:?usage: pilot-join.sh OWNER [CODE]}" code="${2:-}"; name_ok "$owner"
if [ -z "$code" ]; then
  on_vm "vm-agent-$owner" "systemctl stop agentvm-firstboot 2>/dev/null; $(as_agent 'conde login --server https://dakai.conde.cloud --code 2>&1 | grep -E "https://" | head -1')"
  exit 0
fi
[[ $code =~ ^[A-Za-z0-9-]{8,9}$ ]] || { echo "the conde code looks like ABCD-1234" >&2; exit 64; }   # conde validates it
# First boot writes agent.env with its BOARD from machine.env; the session hook publishes, conde login does not.
on_vm "vm-agent-$owner" "$(as_agent "conde login --server https://dakai.conde.cloud --code $code 2>&1 | tail -n 1; conde boards | tail -n +2")
printf '%s=%q\\n' OWNER $(printf '%q' "$owner") FULL_NAME $(printf '%q' "$owner/praxy") BOARD dakai > /var/lib/agentvm/machine.env
date -u +%FT%TZ > /var/lib/agentvm/enrolled
systemctl start agentvm-firstboot; journalctl -u agentvm-firstboot --no-pager -o cat | grep agentvm-firstboot | tail -2"
