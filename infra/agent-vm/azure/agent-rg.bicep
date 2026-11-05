// The agent resource group (conde-vm, which already holds vnet-agent-vms, nat-agent-vms and nsg-agent-vms):
// what the conde server may do in it, SSH from the VPN, and the agent side of the hub peering.
//
//   az deployment group create -g conde-vm -f infra/agent-vm/azure/agent-rg.bicep
//
// Deploy hub-peering.bicep first: the agent side uses the hub's gateway, which needs the hub side in place.
//
// The server's identity id-conde gets Contributor here (VMs, NICs, disks, deployments of the template spec) and
// Role Based Access Control Administrator here, conditioned so it can only assign or remove one role, Virtual
// Machine Administrator Login, and only to users: role assignment is the most sensitive write in a subscription.

param condePrincipalId string = '9e00850b-c40c-42a9-a58e-e2bc3118a3a2'

@description('The hub VPN gateway\'s client address pool (vpngw-native-hub).')
param vpnClientPool string = '172.16.0.0/22'

param hubVnetId string = resourceId('rg-native-hub', 'Microsoft.Network/virtualNetworks', 'vnet-native-hub')

@description('False to leave the peering out (e.g. before the hub side exists).')
param peerToHub bool = true

var contributor = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'b24988ac-6180-42a0-ab88-20f7382dd24c')
var rbacAdmin = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'f58310d9-a9f6-439a-9e8d-f62e7b41a168')
var vmAdminLoginGuid = '1c0163c0-47e6-4577-8991-ea5c82e286e4'

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

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: 'vnet-agent-vms'
}

resource toHub 'Microsoft.Network/virtualNetworks/virtualNetworkPeerings@2024-05-01' = if (peerToHub) {
  parent: vnet
  name: 'peer-vnet-agent-vms-to-hub'
  properties: {
    remoteVirtualNetwork: { id: hubVnetId }
    allowVirtualNetworkAccess: true
    allowForwardedTraffic: true
    useRemoteGateways: true
  }
}

resource serverContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, condePrincipalId, contributor)
  properties: {
    roleDefinitionId: contributor
    principalId: condePrincipalId
    principalType: 'ServicePrincipal'
    description: 'conde server: deploys agent VMs from the template spec agent-vm'
  }
}

resource serverAssignsLogin 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, condePrincipalId, rbacAdmin)
  properties: {
    roleDefinitionId: rbacAdmin
    principalId: condePrincipalId
    principalType: 'ServicePrincipal'
    description: 'conde server: may assign and remove only Virtual Machine Administrator Login, only to users'
    conditionVersion: '2.0'
    condition: '((!(ActionMatches{\'Microsoft.Authorization/roleAssignments/write\'})) OR (@Request[Microsoft.Authorization/roleAssignments:RoleDefinitionId] ForAnyOfAnyValues:GuidEquals {${vmAdminLoginGuid}} AND @Request[Microsoft.Authorization/roleAssignments:PrincipalType] ForAnyOfAnyValues:StringEqualsIgnoreCase {\'User\'})) AND ((!(ActionMatches{\'Microsoft.Authorization/roleAssignments/delete\'})) OR (@Resource[Microsoft.Authorization/roleAssignments:RoleDefinitionId] ForAnyOfAnyValues:GuidEquals {${vmAdminLoginGuid}}))'
  }
}
