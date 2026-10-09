# Release binaries (macOS/Linux builds from the GitHub release workflow of DakaiGroup/conde): a blob container with
# anonymous read, written by the workflow through a GitHub OIDC identity. The server redirects /install.sh and
# /dist/<file> to it (CONDE_DIST_BASE_URL).
resource "azurerm_storage_account" "dist" {
  name                            = "stconde"
  location                        = azurerm_resource_group.this.location
  resource_group_name             = azurerm_resource_group.this.name
  account_tier                    = "Standard"
  account_replication_type        = "LRS"
  account_kind                    = "StorageV2"
  min_tls_version                 = "TLS1_2"
  allow_nested_items_to_be_public = true
  shared_access_key_enabled       = false
  tags                            = local.tags
}

resource "azurerm_storage_container" "dist" {
  name                  = "dist"
  storage_account_id    = azurerm_storage_account.dist.id
  container_access_type = "blob" # anonymous GET on blobs, no listing
}

# GitHub Actions of DakaiGroup/conde (repo id 1411036719, org id 65165889: immutable subject format). Entra
# federated credentials take no wildcard, so the workflow runs its upload step in the GitHub environment "release"
# (one subject for tags and manual runs alike).
resource "azuread_application" "release" {
  display_name = "${local.name}-github-release"
  notes        = "Release workflow of DakaiGroup/conde: uploads binaries to stconde/dist."
}

resource "azuread_service_principal" "release" {
  client_id = azuread_application.release.client_id
}

resource "azuread_application_federated_identity_credential" "release" {
  application_id = azuread_application.release.id
  display_name   = "github-environment-release"
  audiences      = ["api://AzureADTokenExchange"]
  issuer         = "https://token.actions.githubusercontent.com"
  subject        = "repo:DakaiGroup@65165889/conde@1411036719:environment:release"
}

resource "azurerm_role_assignment" "release_dist" {
  scope                = azurerm_storage_container.dist.id
  role_definition_name = "Storage Blob Data Contributor"
  principal_id         = azuread_service_principal.release.object_id
}

output "github" {
  value = {
    AZURE_CLIENT_ID       = azuread_application.release.client_id
    AZURE_TENANT_ID       = data.azurerm_client_config.current.tenant_id
    AZURE_SUBSCRIPTION_ID = local.subscription_id
    DIST_ACCOUNT          = azurerm_storage_account.dist.name
    DIST_BASE_URL         = "${azurerm_storage_account.dist.primary_blob_endpoint}${azurerm_storage_container.dist.name}"
  }
}
