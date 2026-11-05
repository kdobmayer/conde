// The agent resource group (conde-vm, which already holds vnet-agent-vms, nat-agent-vms and nsg-agent-vms):
// what the conde server may do in it, inbound traffic, and the agent side of the hub peering.
//
//   az deployment group create -g conde-vm -f infra/agent-vm/azure/agent-rg.bicep
//
// The hub side of the peering is native-infra's (terraform/network.tf, hub_to_this, like every spoke) and has to
// exist first: the agent side uses the hub's gateway. Until then deploy with -p peerToHub=false.
//
// The server's identity id-conde gets two roles here, nothing more:
//   - Agent VM Deployer, a custom role with only what the template spec and the disable route use: VMs, their
//     extensions, NICs and OS disks; joining the subnet; deployments; reading the template spec. Not Contributor:
//     that would also give run-command and extension writes (a root shell on every agent VM, so every person's
//     Claude login and GitHub token), disk export, and writes on the shared VNet, NAT gateway and NSG.
//   - Role Based Access Control Administrator, conditioned so it can only assign or remove one role, Virtual
//     Machine Administrator Login, and only for users.

param condePrincipalId string = '9e00850b-c40c-42a9-a58e-e2bc3118a3a2'

@description('The hub VPN gateway\'s client address pool (vpngw-native-hub).')
param vpnClientPool string = '172.16.0.0/22'

param hubVnetId string = resourceId('rg-native-hub', 'Microsoft.Network/virtualNetworks', 'vnet-native-hub')

@description('False until the hub side of the peering exists (native-infra).')
param peerToHub bool = true

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

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: 'vnet-agent-vms'
}

resource toHub 'Microsoft.Network/virtualNetworks/virtualNetworkPeerings@2024-05-01' = if (peerToHub) {
  parent: vnet
  name: 'peer-vnet-agent-vms-to-hub'
  properties: {
    remoteVirtualNetwork: { id: hubVnetId }
    allowVirtualNetworkAccess: true
    allowForwardedTraffic: false
    useRemoteGateways: true
  }
}

resource deployerRole 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'agent-vm-deployer')
  properties: {
    roleName: 'Agent VM Deployer (${resourceGroup().name})'
    description: 'conde server: deploys and deletes agent VMs from the template spec agent-vm. No run-command, no disk export, no network writes beyond NICs.'
    type: 'CustomRole'
    assignableScopes: [resourceGroup().id]
    permissions: [
      {
        actions: [
          'Microsoft.Resources/subscriptions/resourceGroups/read'
          'Microsoft.Resources/deployments/*'
          'Microsoft.Resources/templateSpecs/read'
          'Microsoft.Resources/templateSpecs/versions/read'
          'Microsoft.Compute/virtualMachines/read'
          'Microsoft.Compute/virtualMachines/write'
          'Microsoft.Compute/virtualMachines/delete'
          'Microsoft.Compute/virtualMachines/instanceView/read'
          'Microsoft.Compute/virtualMachines/extensions/read'
          'Microsoft.Compute/virtualMachines/extensions/write'
          'Microsoft.Compute/virtualMachines/extensions/delete'
          'Microsoft.Compute/disks/read'
          'Microsoft.Compute/disks/write'
          'Microsoft.Compute/disks/delete'
          'Microsoft.Compute/locations/*/read'
          'Microsoft.Network/networkInterfaces/read'
          'Microsoft.Network/networkInterfaces/write'
          'Microsoft.Network/networkInterfaces/delete'
          'Microsoft.Network/networkInterfaces/join/action'
          'Microsoft.Network/virtualNetworks/read'
          'Microsoft.Network/virtualNetworks/subnets/read'
          'Microsoft.Network/virtualNetworks/subnets/join/action'
          'Microsoft.Network/locations/*/read'
        ]
        notActions: []
      }
    ]
  }
}

resource serverDeploys 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, condePrincipalId, deployerRole.id)
  properties: {
    roleDefinitionId: deployerRole.id
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
    description: 'conde server: may assign and remove only Virtual Machine Administrator Login, only for users'
    conditionVersion: '2.0'
    condition: '((!(ActionMatches{\'Microsoft.Authorization/roleAssignments/write\'})) OR (@Request[Microsoft.Authorization/roleAssignments:RoleDefinitionId] ForAnyOfAnyValues:GuidEquals {${vmAdminLoginGuid}} AND @Request[Microsoft.Authorization/roleAssignments:PrincipalType] ForAnyOfAnyValues:StringEqualsIgnoreCase {\'User\'})) AND ((!(ActionMatches{\'Microsoft.Authorization/roleAssignments/delete\'})) OR (@Resource[Microsoft.Authorization/roleAssignments:RoleDefinitionId] ForAnyOfAnyValues:GuidEquals {${vmAdminLoginGuid}} AND @Resource[Microsoft.Authorization/roleAssignments:PrincipalType] ForAnyOfAnyValues:StringEqualsIgnoreCase {\'User\'}))'
  }
}
