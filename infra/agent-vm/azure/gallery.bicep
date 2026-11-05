// The image definition for agent VMs in the existing gallery, and who may use it. Resource group RG-NATIVE-LANES:
//
//   az deployment group create -g RG-NATIVE-LANES -f infra/agent-vm/azure/gallery.bicep -p builderPrincipalId=<sp object id>
//
// Generalized (praxia-lane next to it is specialized and is not touched). The image workflow's identity writes
// versions of this definition only (no role on the gallery itself, so nothing on praxia-lane); the conde server's
// identity reads them to create VMs.

@description('Object id of the service principal of the image workflow\'s Entra app (azure/image-builder.sh).')
param builderPrincipalId string

@description('Object id of the conde server\'s identity id-conde.')
param condePrincipalId string = '9e00850b-c40c-42a9-a58e-e2bc3118a3a2'

@description('Must be the gallery\'s own location (galNativeLanes: westeurope), not the resource group\'s. Bicep cannot read an existing resource\'s location at the start of a deployment, hence a parameter.')
param galleryLocation string = 'westeurope'

var reader = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'acdd72a7-3385-48ef-bd42-f606fba81ae7')
var contributor = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'b24988ac-6180-42a0-ab88-20f7382dd24c')

resource gallery 'Microsoft.Compute/galleries@2023-07-03' existing = {
  name: 'galNativeLanes'
}

resource agentVm 'Microsoft.Compute/galleries/images@2023-07-03' = {
  parent: gallery
  name: 'agent-vm'
  location: galleryLocation
  properties: {
    description: 'Always-on agent VM: Ubuntu 24.04, Claude Code, conde, git, gh, uv, Chromium (conde infra/agent-vm)'
    osType: 'Linux'
    osState: 'Generalized'
    hyperVGeneration: 'V2'
    architecture: 'x64'
    identifier: {
      publisher: 'DakaiGroup'
      offer: 'agent-vm'
      sku: 'ubuntu-24_04'
    }
  }
}

resource builderWritesVersions 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(agentVm.id, builderPrincipalId, contributor)
  scope: agentVm
  properties: {
    roleDefinitionId: contributor
    principalId: builderPrincipalId
    principalType: 'ServicePrincipal'
  }
}

resource condeReadsImage 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(agentVm.id, condePrincipalId, reader)
  scope: agentVm
  properties: {
    roleDefinitionId: reader
    principalId: condePrincipalId
    principalType: 'ServicePrincipal'
  }
}

output imageDefinitionId string = agentVm.id
