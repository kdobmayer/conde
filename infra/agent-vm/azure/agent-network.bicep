// The agent VNet's network: inbound rules and the agent side of the hub peering. Resource group conde-vm.
//
//   az deployment group create -g conde-vm -f infra/agent-vm/azure/agent-network.bicep
//
// The hub side (peer-hub-to-vnet-agent-vms, gateway transit) is native-infra's Terraform (terraform/hub/network.tf);
// it needs the role in hub-peer-access.bicep and has to exist first. The rules go with the peering: without the
// denies, the defaults AllowVnetInBound and AllowVnetOutBound open every port between the agent VMs, the hub and
// every VPN client.
// DNS stays Azure's default: agents need public names only, and the hub resolver would be one more dependency of
// every always-on agent.

@description('The hub VPN gateway\'s client address pool (vpngw-native-hub).')
param vpnClientPool string = '172.16.0.0/22'

param hubVnetId string = resourceId('rg-native-hub', 'Microsoft.Network/virtualNetworks', 'vnet-native-hub')

@description('False until the hub side of the peering exists (native-infra).')
param peerToHub bool = true

resource nsg 'Microsoft.Network/networkSecurityGroups@2024-05-01' existing = {
  name: 'nsg-agent-vms'
}

resource sshFromVpn 'Microsoft.Network/networkSecurityGroups/securityRules@2024-05-01' = {
  parent: nsg
  name: 'allow-ssh-from-vpn'
  properties: {
    priority: 200
    direction: 'Inbound'
    access: 'Allow'
    protocol: 'Tcp'
    sourceAddressPrefix: vpnClientPool
    sourcePortRange: '*'
    destinationAddressPrefix: 'VirtualNetwork'
    destinationPortRange: '22'
    description: 'Entra SSH for power users over the hub VPN (P2S client pool)'
  }
}

// The default AllowVnetInBound (65000) would open every port to the peered hub, every VPN client and the other
// agent VMs. Agents talk through the board, so nothing inbound from the VNet is needed beyond the SSH rule above.
resource denyVnetIn 'Microsoft.Network/networkSecurityGroups/securityRules@2024-05-01' = {
  parent: nsg
  name: 'deny-vnet-inbound'
  properties: {
    priority: 4000
    direction: 'Inbound'
    access: 'Deny'
    protocol: '*'
    sourceAddressPrefix: 'VirtualNetwork'
    sourcePortRange: '*'
    destinationAddressPrefix: '*'
    destinationPortRange: '*'
    description: 'Only SSH from the VPN (allow-ssh-from-vpn); agents talk through the conde board'
  }
}

// The agent needs nothing inside the VNet either: no hub endpoints (AKS API, Key Vault, Postgres), no resolver, no
// VPN clients, no other agent VM. Its traffic goes to the internet through the NAT gateway. Azure DNS (168.63.129.16)
// and the metadata service (169.254.169.254) are not in the VirtualNetwork tag.
resource denyVnetOut 'Microsoft.Network/networkSecurityGroups/securityRules@2024-05-01' = {
  parent: nsg
  name: 'deny-vnet-outbound'
  properties: {
    priority: 4000
    direction: 'Outbound'
    access: 'Deny'
    protocol: '*'
    sourceAddressPrefix: '*'
    sourcePortRange: '*'
    destinationAddressPrefix: 'VirtualNetwork'
    destinationPortRange: '*'
    description: 'Agents reach the internet only (NAT); nothing inside the VNet or behind the hub'
  }
}

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: 'vnet-agent-vms'
}

// After the rules: the peering must not connect before the denies exist.
resource toHub 'Microsoft.Network/virtualNetworks/virtualNetworkPeerings@2024-05-01' = if (peerToHub) {
  parent: vnet
  name: 'peer-vnet-agent-vms-to-hub'
  dependsOn: [
    sshFromVpn
    denyVnetIn
    denyVnetOut
  ]
  properties: {
    remoteVirtualNetwork: { id: hubVnetId }
    allowVirtualNetworkAccess: true
    allowForwardedTraffic: false
    useRemoteGateways: true
  }
}
