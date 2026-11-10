# The release workflow's image job (GitHub OIDC, no stored secret) pushes the server image to crnative beside Artifact
# Registry, so a deploy here only points ca-conde at the digest. AcrPush on the shared registry and nothing else.
resource "azuread_application" "image" {
  display_name = "${local.name}-github-image"
  notes        = "Release workflow of kdobmayer/conde: pushes the server image to crnative."
}

resource "azuread_service_principal" "image" {
  client_id = azuread_application.image.client_id
}

# The repo uses GitHub's immutable subject (owner and repo ids); the `image` environment admits v* tags only.
resource "azuread_application_federated_identity_credential" "image" {
  application_id = azuread_application.image.id
  display_name   = "github-environment-image"
  audiences      = ["api://AzureADTokenExchange"]
  issuer         = "https://token.actions.githubusercontent.com"
  subject        = "repo:kdobmayer@22565526/conde@1436388412:environment:image"
}

resource "azurerm_role_assignment" "image_acr_push" {
  scope                = data.azurerm_container_registry.shared.id
  role_definition_name = "AcrPush"
  principal_id         = azuread_service_principal.image.object_id
  principal_type       = "ServicePrincipal"
}

# Variables of the GitHub environment `image`.
output "github_image" {
  value = {
    AZURE_CLIENT_ID       = azuread_application.image.client_id
    AZURE_TENANT_ID       = data.azurerm_client_config.current.tenant_id
    AZURE_SUBSCRIPTION_ID = local.subscription_id
  }
}
