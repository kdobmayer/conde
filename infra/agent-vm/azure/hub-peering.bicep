// The hub side of the agent VNet's peering, written into vnet-native-hub (resource group rg-native-hub), the same
// shape as native-infra's spoke peerings (terraform/network.tf, hub_to_this): gateway transit gives VPN clients
// the agent VNet's routes and the agent VNet the client pool.
//
//   az deployment group create -g rg-native-hub -f infra/agent-vm/azure/hub-peering.bicep
//
// The hub is Terraform-managed in native-infra; this peering is a separate child resource that the hub's own
// Terraform does not manage, as with the dro and native spokes. Windows VPN users download their client profile
// again afterwards: P2S clients learn new routes only from a fresh profile.

param agentVnetId string = resourceId('conde-vm', 'Microsoft.Network/virtualNetworks', 'vnet-agent-vms')

resource hub 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: 'vnet-native-hub'
}

resource hubToAgents 'Microsoft.Network/virtualNetworks/virtualNetworkPeerings@2024-05-01' = {
  parent: hub
  name: 'peer-hub-to-vnet-agent-vms'
  properties: {
    remoteVirtualNetwork: { id: agentVnetId }
    allowVirtualNetworkAccess: true
    allowForwardedTraffic: true
    allowGatewayTransit: true
  }
}
