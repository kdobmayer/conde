// Lets native-infra's hub pipeline create the hub side of the agent VNet's peering (peer-hub-to-vnet-agent-vms,
// native-infra terraform/hub/network.tf). Creating a peering needs the peer action on the remote VNet too, so the
// hub's deployer identity (id-native-hub-deployer) gets exactly that on vnet-agent-vms, plus reading the VNet for its
// data source: not Network Contributor, which would also let it change the agent VNet's subnets, address space and
// other peerings. (The hub side reads its own peering on the hub VNet, so no peering read here.)
//
//   az deployment group create -g conde-vm -f infra/agent-vm/azure/hub-peer-access.bicep
//
// Separate from agent-rg.bicep, so it can go in on its own.

@description('Object id of the managed identity id-native-hub-deployer (the hub stack\'s Terraform identity).')
param hubDeployerPrincipalId string = 'd36e9fa6-79de-46f8-be94-42b271371840'

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: 'vnet-agent-vms'
}

resource peerRole 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'agent-vnet-peer')
  properties: {
    roleName: 'Agent VNet Peer (${resourceGroup().name})'
    description: 'Read vnet-agent-vms and peer another VNet with it; nothing else.'
    type: 'CustomRole'
    assignableScopes: [resourceGroup().id]
    permissions: [
      {
        actions: [
          'Microsoft.Network/virtualNetworks/read'
          'Microsoft.Network/virtualNetworks/peer/action'
        ]
        notActions: []
      }
    ]
  }
}

resource hubDeployerPeers 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(vnet.id, hubDeployerPrincipalId, peerRole.id)
  scope: vnet
  properties: {
    roleDefinitionId: peerRole.id
    principalId: hubDeployerPrincipalId
    principalType: 'ServicePrincipal'
    description: 'native-infra hub pipeline: the hub side of the agent VNet peering'
  }
}
