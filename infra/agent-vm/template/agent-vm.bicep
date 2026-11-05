// One person's agent VM. Published as the template spec `agent-vm` in the agent resource group and deployed
// by the conde server, one named deployment per person (`agent-vm-<owner>`), idempotent on repeat:
//
//   az ts create -g conde-vm -n agent-vm -v <version> -f infra/agent-vm/template/agent-vm.bicep
//
// The VM has no public IP (outbound through the subnet's NAT gateway), a system-assigned identity (the proof
// it presents to the conde server at first boot, and what Entra SSH needs), Entra SSH login for the person,
// and no role of its own over anything. Deleting the VM deletes its NIC and disk.
//
// Output `principalId` is the identity's object id: the server keys the machine record on it (the token's
// `oid` claim at the mint call).

@description('The person\'s conde board user name, e.g. levente-szabo. The VM is vm-agent-<owner>.')
@minLength(2)
@maxLength(40)
param owner string

@description('The person\'s Entra object id: gets Virtual Machine Administrator Login on this VM (Entra SSH).')
param ownerObjectId string

@allowed([
  'Standard_D2as_v5'
  'Standard_B2as_v2'
])
param vmSize string = 'Standard_D2as_v5'

@description('Gallery image definition (newest version) or one image version.')
param imageId string = resourceId('RG-NATIVE-LANES', 'Microsoft.Compute/galleries/images', 'galNativeLanes', 'agent-vm')

param subnetId string = resourceId('conde-vm', 'Microsoft.Network/virtualNetworks/subnets', 'vnet-agent-vms', 'snet-agent-vms')

@description('The conde server the VM enrols with.')
param condeServer string = 'https://dakai.conde.cloud'

@description('Resource (audience) of the identity token the VM presents: the conde application\'s id URI.')
param condeResource string = 'api://d87a5254-3121-4d3b-bd1e-b902b3bcbba0'

@description('Public key for the image\'s admin user. Azure requires one; nobody needs its private half (people log in with Entra SSH), so the server may pass a throwaway key.')
param adminSshPublicKey string

param location string = resourceGroup().location

param createdAt string = utcNow('u')

var vmName = 'vm-agent-${owner}'
var vmAdminLoginRole = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '1c0163c0-47e6-4577-8991-ea5c82e286e4')
var tags = {
  'agentvm-person': owner
  'agentvm-created': createdAt
}

resource nic 'Microsoft.Network/networkInterfaces@2024-05-01' = {
  name: 'nic-${vmName}'
  location: location
  tags: tags
  properties: {
    ipConfigurations: [
      {
        name: 'ipconfig1'
        properties: {
          subnet: { id: subnetId }
          privateIPAllocationMethod: 'Dynamic'
        }
      }
    ]
  }
}

resource vm 'Microsoft.Compute/virtualMachines@2024-07-01' = {
  name: vmName
  location: location
  tags: tags
  identity: { type: 'SystemAssigned' }
  properties: {
    hardwareProfile: { vmSize: vmSize }
    storageProfile: {
      imageReference: { id: imageId }
      osDisk: {
        name: 'osdisk-${vmName}'
        createOption: 'FromImage'
        deleteOption: 'Delete'
        diskSizeGB: 64
        managedDisk: { storageAccountType: 'StandardSSD_LRS' }
      }
    }
    osProfile: {
      computerName: vmName
      adminUsername: 'azureuser'
      linuxConfiguration: {
        disablePasswordAuthentication: true
        ssh: {
          publicKeys: [
            {
              path: '/home/azureuser/.ssh/authorized_keys'
              keyData: adminSshPublicKey
            }
          ]
        }
      }
    }
    networkProfile: {
      networkInterfaces: [
        {
          id: nic.id
          properties: { deleteOption: 'Delete' }
        }
      ]
    }
    diagnosticsProfile: {
      bootDiagnostics: { enabled: true }
    }
    userData: base64(string({
      condeServer: condeServer
      condeResource: condeResource
    }))
  }
}

resource entraSsh 'Microsoft.Compute/virtualMachines/extensions@2024-07-01' = {
  parent: vm
  name: 'AADSSHLoginForLinux'
  location: location
  properties: {
    publisher: 'Microsoft.Azure.ActiveDirectory'
    type: 'AADSSHLoginForLinux'
    typeHandlerVersion: '1.0'
    autoUpgradeMinorVersion: true
  }
}

resource ownerLogin 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(vm.id, ownerObjectId, vmAdminLoginRole)
  scope: vm
  properties: {
    roleDefinitionId: vmAdminLoginRole
    principalId: ownerObjectId
    principalType: 'User'
  }
}

output vmName string = vm.name
output principalId string = vm.identity.principalId
