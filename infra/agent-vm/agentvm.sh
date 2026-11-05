#!/usr/bin/env bash
# Operator side: create, drive and delete a person's agent VM. The trigger is manual for now (later the
# conde web sign-in). Needs `az` signed in to the subscription below.
#
#   ./agentvm.sh create PERSON        network once, then the VM; cloud-init runs bootstrap.sh
#   ./agentvm.sh run PERSON STEP ...  one `agentvm` step on the VM (status, claude-login-start, ...)
#   ./agentvm.sh delete PERSON        the VM and its disk and NIC (the network stays)
#
# PERSON is the person's handle, also the VM's user name: lowercase letters, digits and dashes.
set -euo pipefail

SUB="${AGENTVM_SUBSCRIPTION:-60df47fc-ce8f-4f52-a970-43a412d78007}"
RG="${AGENTVM_RG:-vm-conde-test}"
LOC="${AGENTVM_LOCATION:-westeurope}"
SIZE="${AGENTVM_SIZE:-Standard_B2s}"
IMAGE="Canonical:ubuntu-24_04-lts:server:latest"
VNET=vnet-agent-vms SUBNET=snet-agent-vms NAT=nat-agent-vms NATIP=pip-nat-agent-vms NSG=nsg-agent-vms
KEY="${AGENTVM_SSH_KEY:-$HOME/.config/agentvm/id_ed25519}"
HERE="$(cd "$(dirname "$0")" && pwd)"
az() { command az "$@" --subscription "$SUB"; }

person_ok() { [[ "$1" =~ ^[a-z][a-z0-9-]{0,30}$ ]] || { echo "PERSON must match ^[a-z][a-z0-9-]{0,30}\$"; exit 64; }; }
vm_name() { echo "vm-agent-$1"; }

# One network for every agent VM: no public IPs, outbound through a NAT gateway (remote control and the
# conde bridge only dial out), an NSG with no inbound rule beyond Azure's defaults.
ensure_network() {
  az group create -n "$RG" -l "$LOC" -o none
  if ! az network vnet show -g "$RG" -n "$VNET" -o none 2>/dev/null; then
    az network public-ip create -g "$RG" -n "$NATIP" --sku Standard -o none
    az network nat gateway create -g "$RG" -n "$NAT" --public-ip-addresses "$NATIP" -o none
    az network nsg create -g "$RG" -n "$NSG" -o none
    az network vnet create -g "$RG" -n "$VNET" --address-prefixes 10.150.0.0/16 \
      --subnet-name "$SUBNET" --subnet-prefixes 10.150.0.0/24 -o none
    az network vnet subnet update -g "$RG" --vnet-name "$VNET" -n "$SUBNET" \
      --nat-gateway "$NAT" --network-security-group "$NSG" --default-outbound-access false -o none
  fi
}

cloud_init() {
  local person="$1"
  b64() { base64 -w0 < "$1"; }
  cat <<EOF
#cloud-config
write_files:
  - path: /etc/agentvm.conf
    permissions: "0644"
    content: |
      AGENT_USER=$person
  - path: /usr/local/lib/agentvm/bootstrap.sh
    permissions: "0755"
    encoding: b64
    content: $(b64 "$HERE/bootstrap.sh")
  - path: /usr/local/lib/agentvm/run-agent
    permissions: "0755"
    encoding: b64
    content: $(b64 "$HERE/run-agent")
  - path: /usr/local/lib/agentvm/conde-register-hook
    permissions: "0755"
    encoding: b64
    content: $(b64 "$HERE/conde-register-hook")
  - path: /usr/local/lib/agentvm/claude-agent.service
    permissions: "0644"
    encoding: b64
    content: $(b64 "$HERE/claude-agent.service")
  - path: /usr/local/bin/agentvm
    permissions: "0755"
    encoding: b64
    content: $(b64 "$HERE/agentvm")
runcmd:
  - [bash, /usr/local/lib/agentvm/bootstrap.sh]
EOF
}

cmd_create() {
  local person="${1:?usage: agentvm.sh create PERSON}"; person_ok "$person"
  [ -f "$KEY" ] || { mkdir -p "$(dirname "$KEY")"; ssh-keygen -q -t ed25519 -N '' -C agentvm-operator -f "$KEY"; }
  ensure_network
  local ci; ci=$(mktemp); trap 'rm -f "$ci"' RETURN
  cloud_init "$person" > "$ci"
  az vm create -g "$RG" -n "$(vm_name "$person")" --image "$IMAGE" --size "$SIZE" \
    --vnet-name "$VNET" --subnet "$SUBNET" --public-ip-address "" --nsg "" \
    --admin-username azureuser --ssh-key-values "$KEY.pub" \
    --os-disk-size-gb 64 --custom-data "$ci" --tags "agentvm-person=$person" \
    --query '{name:name, privateIp:privateIpAddress, powerState:powerState}' -o table
  echo "cloud-init is installing now (5-10 min). Check with: $0 run $person status"
}

cmd_run() {
  local person="${1:?usage: agentvm.sh run PERSON STEP [ARGS]}"; shift; person_ok "$person"
  local script="agentvm"; for a in "$@"; do script+=" $(printf '%q' "$a")"; done
  az vm run-command invoke -g "$RG" -n "$(vm_name "$person")" --command-id RunShellScript \
    --scripts "$script" --query 'value[0].message' -o tsv
}

cmd_delete() {
  local person="${1:?usage: agentvm.sh delete PERSON}"; person_ok "$person"
  local vm; vm=$(vm_name "$person")
  local disk nic
  disk=$(az vm show -g "$RG" -n "$vm" --query storageProfile.osDisk.managedDisk.id -o tsv)
  nic=$(az vm show -g "$RG" -n "$vm" --query 'networkProfile.networkInterfaces[0].id' -o tsv)
  az vm delete -g "$RG" -n "$vm" --yes -o none
  az network nic delete --ids "$nic" -o none
  az disk delete --ids "$disk" --yes -o none
  echo "deleted $vm, its NIC and its disk"
}

case "${1:-}" in
  create) shift; cmd_create "$@" ;;
  run) shift; cmd_run "$@" ;;
  delete) shift; cmd_delete "$@" ;;
  *) sed -n '2,9p' "$0"; exit 64 ;;
esac
