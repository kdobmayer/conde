# conde (the cloud successor of the kond agent message board, a Rust server) on Azure Container Apps with a managed
# Postgres, in its own resource group of the shared Dakai subscription (60df47fc, West Europe), nothing in
# native-infra. Local Terraform state next to this file; applied by hand from this directory. The image is built
# locally into crnative (no GitHub repo). No secret value is in this state: the Postgres password and the admin key
# are ephemeral random values written write-only into the vault and read back from there.
provider "azurerm" {
  subscription_id     = local.subscription_id
  storage_use_azuread = true
  features {}
}

provider "azuread" {}

data "azurerm_client_config" "current" {}

locals {
  subscription_id = "60df47fc-ce8f-4f52-a970-43a412d78007"
  location        = "westeurope"
  name            = "conde"
  rg              = "rg-${local.name}"
  host            = "conde"
  domain          = "nativeai.space"
  fqdn            = "${local.host}.${local.domain}"
  owner_object_id = "da79aae8-c6e1-4f10-9826-42aeeab0c371" # krisztian.dobmayer@dakai.io: writes vault secrets by hand
  tags = {
    project = "conde"
  }
}

resource "azurerm_resource_group" "this" {
  name     = local.rg
  location = local.location
  tags     = local.tags
}

resource "azurerm_log_analytics_workspace" "this" {
  name                = "log-${local.name}"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  sku                 = "PerGB2018"
  retention_in_days   = 30
  tags                = local.tags
}

resource "azurerm_user_assigned_identity" "app" {
  name                = "id-${local.name}"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  tags                = local.tags
}

resource "azurerm_key_vault" "this" {
  name                       = "kv-${local.name}"
  location                   = azurerm_resource_group.this.location
  resource_group_name        = azurerm_resource_group.this.name
  tenant_id                  = data.azurerm_client_config.current.tenant_id
  sku_name                   = "standard"
  rbac_authorization_enabled = true
  purge_protection_enabled   = false
  soft_delete_retention_days = 7
  tags                       = local.tags
}

resource "azurerm_role_assignment" "kv_app" {
  scope                = azurerm_key_vault.this.id
  role_definition_name = "Key Vault Secrets User"
  principal_id         = azurerm_user_assigned_identity.app.principal_id
}

resource "azurerm_role_assignment" "kv_owner" {
  scope                = azurerm_key_vault.this.id
  role_definition_name = "Key Vault Secrets Officer"
  principal_id         = local.owner_object_id
  principal_type       = "User"
}

data "azurerm_container_registry" "shared" {
  name                = "crnative"
  resource_group_name = "rg-native"
}

resource "azurerm_role_assignment" "acr_pull" {
  scope                = data.azurerm_container_registry.shared.id
  role_definition_name = "AcrPull"
  principal_id         = azurerm_user_assigned_identity.app.principal_id
}

# Role assignments return before they take effect; the secret writes and the app's Key Vault references need them.
resource "time_sleep" "rbac" {
  create_duration = "120s"
  triggers = {
    kv_app   = azurerm_role_assignment.kv_app.id
    kv_owner = azurerm_role_assignment.kv_owner.id
    acr_pull = azurerm_role_assignment.acr_pull.id
  }
}

# ── Network: the environment and the Postgres private endpoint share one VNet (10.140 is meetings) ─────────────────
resource "azurerm_virtual_network" "this" {
  name                = "vnet-${local.name}"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  address_space       = ["10.141.0.0/16"]
  tags                = local.tags
}

resource "azurerm_subnet" "aca" {
  name                 = "snet-aca"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.141.0.0/23"]

  delegation {
    name = "aca"
    service_delegation {
      name    = "Microsoft.App/environments"
      actions = ["Microsoft.Network/virtualNetworks/subnets/join/action"]
    }
  }
}

resource "azurerm_subnet" "endpoints" {
  name                 = "snet-endpoints"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.141.4.0/24"]
}

resource "azurerm_container_app_environment" "this" {
  name                               = "cae-${local.name}"
  location                           = azurerm_resource_group.this.location
  resource_group_name                = azurerm_resource_group.this.name
  log_analytics_workspace_id         = azurerm_log_analytics_workspace.this.id
  logs_destination                   = "log-analytics"
  infrastructure_subnet_id           = azurerm_subnet.aca.id
  infrastructure_resource_group_name = "${local.rg}-infra"
  tags                               = local.tags

  workload_profile {
    name                  = "Consumption"
    workload_profile_type = "Consumption"
    minimum_count         = 0
    maximum_count         = 0
  }
}

# ── Postgres: Flexible Server behind a private endpoint, public access off, B1ms, single zone, no HA ───────────────
resource "azurerm_private_dns_zone" "postgres" {
  name                = "privatelink.postgres.database.azure.com"
  resource_group_name = azurerm_resource_group.this.name
  tags                = local.tags
}

resource "azurerm_private_dns_zone_virtual_network_link" "postgres" {
  name                = "vnet-${local.name}"
  private_dns_zone_id = azurerm_private_dns_zone.postgres.id
  virtual_network_id  = azurerm_virtual_network.this.id
  tags                = local.tags
}

resource "azurerm_postgresql_flexible_server" "this" {
  name                              = "psql-${local.name}"
  location                          = azurerm_resource_group.this.location
  resource_group_name               = azurerm_resource_group.this.name
  version                           = "18"
  sku_name                          = "B_Standard_B1ms"
  storage_mb                        = 32768
  backup_retention_days             = 7
  zone                              = "1"
  public_network_access_enabled     = false
  administrator_login               = "conde"
  administrator_password_wo         = ephemeral.azurerm_key_vault_secret.db_password.value
  administrator_password_wo_version = var.db_password_version
  tags                              = local.tags

  authentication {
    password_auth_enabled         = true
    active_directory_auth_enabled = false
  }
}

resource "azurerm_private_endpoint" "postgres" {
  name                = "pe-psql-${local.name}"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  subnet_id           = azurerm_subnet.endpoints.id
  tags                = local.tags

  private_service_connection {
    name                           = "psql-${local.name}"
    private_connection_resource_id = azurerm_postgresql_flexible_server.this.id
    subresource_names              = ["postgresqlServer"]
    is_manual_connection           = false
  }

  private_dns_zone_group {
    name                 = "postgres"
    private_dns_zone_ids = [azurerm_private_dns_zone.postgres.id]
  }

  depends_on = [azurerm_private_dns_zone_virtual_network_link.postgres]
}

resource "azurerm_postgresql_flexible_server_database" "conde" {
  name      = "conde"
  server_id = azurerm_postgresql_flexible_server.this.id
  charset   = "UTF8"
  collation = "en_US.utf8"
}

# ── Secrets: generated once into the vault, read back ephemerally; the app gets Key Vault references ───────────────
ephemeral "random_password" "db" {
  length  = 32
  special = false
}

ephemeral "random_password" "admin_key" {
  length  = 48
  special = false
}

resource "azurerm_key_vault_secret" "db_password" {
  name             = "db-password"
  key_vault_id     = azurerm_key_vault.this.id
  value_wo         = ephemeral.random_password.db.result
  value_wo_version = var.db_password_version
  tags             = local.tags

  depends_on = [time_sleep.rbac]
}

# An ephemeral read is opened at plan time unless an input is unknown; this name is unknown exactly when the
# password is created or rotated, which defers the read to the apply, after the write.
resource "terraform_data" "db_password" {
  input            = azurerm_key_vault_secret.db_password.name
  triggers_replace = [var.db_password_version]
}

ephemeral "azurerm_key_vault_secret" "db_password" {
  name         = terraform_data.db_password.output
  key_vault_id = azurerm_key_vault.this.id
}

locals {
  # Explicit CA bundle: verify-full against Azure's chain, the image must carry ca-certificates.
  database_url = "postgresql://${azurerm_postgresql_flexible_server.this.administrator_login}:${ephemeral.azurerm_key_vault_secret.db_password.value}@${azurerm_postgresql_flexible_server.this.fqdn}:5432/${azurerm_postgresql_flexible_server_database.conde.name}?${var.database_url_params}"
}

resource "azurerm_key_vault_secret" "database_url" {
  name             = "database-url"
  key_vault_id     = azurerm_key_vault.this.id
  value_wo         = local.database_url
  value_wo_version = var.db_password_version * 100 + var.database_url_revision
  tags             = local.tags

  depends_on = [time_sleep.rbac]
}

resource "azurerm_key_vault_secret" "admin_key" {
  name             = "admin-key"
  key_vault_id     = azurerm_key_vault.this.id
  value_wo         = ephemeral.random_password.admin_key.result
  value_wo_version = var.admin_key_version
  tags             = local.tags

  depends_on = [time_sleep.rbac]
}

locals {
  app_secrets = {
    (azurerm_key_vault_secret.database_url.name)        = azurerm_key_vault_secret.database_url.versionless_id
    (azurerm_key_vault_secret.admin_key.name)           = azurerm_key_vault_secret.admin_key.versionless_id
    (azurerm_key_vault_secret.entra_client_secret.name) = azurerm_key_vault_secret.entra_client_secret.versionless_id
  }
  secret_env = {
    DATABASE_URL        = azurerm_key_vault_secret.database_url.name
    CONDE_ADMIN_KEY     = azurerm_key_vault_secret.admin_key.name
    ENTRA_CLIENT_SECRET = azurerm_key_vault_secret.entra_client_secret.name
  }
  plain_env = {
    RUST_LOG            = "info"
    CONDE_DIST_BASE_URL = "${azurerm_storage_account.dist.primary_blob_endpoint}${azurerm_storage_container.dist.name}"
    ENTRA_TENANT_ID     = data.azurerm_client_config.current.tenant_id
    ENTRA_CLIENT_ID     = azuread_application.sso.client_id
    ENTRA_REDIRECT_URI  = local.sso_redirect
  }
}

# ── The server: public HTTPS + WebSocket ingress, one replica until the app is proven stateless ────────────────────
resource "azurerm_container_app" "server" {
  name                         = "ca-${local.name}"
  container_app_environment_id = azurerm_container_app_environment.this.id
  resource_group_name          = azurerm_resource_group.this.name
  revision_mode                = "Single"
  workload_profile_name        = "Consumption"
  tags                         = local.tags

  identity {
    type         = "UserAssigned"
    identity_ids = [azurerm_user_assigned_identity.app.id]
  }

  registry {
    server   = data.azurerm_container_registry.shared.login_server
    identity = azurerm_user_assigned_identity.app.id
  }

  dynamic "secret" {
    for_each = local.app_secrets
    content {
      name                = secret.key
      key_vault_secret_id = secret.value
      identity            = azurerm_user_assigned_identity.app.id
    }
  }

  ingress {
    external_enabled           = true
    allow_insecure_connections = false
    target_port                = var.target_port
    transport                  = "auto"
    traffic_weight {
      latest_revision = true
      percentage      = 100
    }
  }

  template {
    min_replicas                     = var.replicas
    max_replicas                     = max(var.replicas, 1)
    termination_grace_period_seconds = 60

    container {
      name   = "conde"
      image  = var.image
      cpu    = 0.5
      memory = "1Gi"

      env {
        name  = "PORT"
        value = tostring(var.target_port)
      }
      dynamic "env" {
        for_each = local.plain_env
        content {
          name  = env.key
          value = env.value
        }
      }
      dynamic "env" {
        for_each = local.secret_env
        content {
          name        = env.key
          secret_name = env.value
        }
      }
    }
  }

  depends_on = [time_sleep.rbac]
}

# ── DNS + custom domain: CNAME to the app, asuid TXT, managed certificate bound once by hand (README) ──────────────
data "azurerm_dns_zone" "public" {
  name                = local.domain
  resource_group_name = "rg-native"
}

resource "azurerm_dns_cname_record" "conde" {
  name                = local.host
  zone_name           = data.azurerm_dns_zone.public.name
  resource_group_name = data.azurerm_dns_zone.public.resource_group_name
  ttl                 = 300
  record              = azurerm_container_app.server.ingress[0].fqdn
  tags                = local.tags
}

resource "azurerm_dns_txt_record" "conde_asuid" {
  name                = "asuid.${local.host}"
  zone_name           = data.azurerm_dns_zone.public.name
  resource_group_name = data.azurerm_dns_zone.public.resource_group_name
  ttl                 = 300
  tags                = local.tags

  record {
    value = azurerm_container_app.server.custom_domain_verification_id
  }
}

resource "azurerm_container_app_custom_domain" "conde" {
  name             = local.fqdn
  container_app_id = azurerm_container_app.server.id

  lifecycle {
    ignore_changes = [certificate_binding_type, container_app_environment_certificate_id]
  }

  depends_on = [azurerm_dns_cname_record.conde, azurerm_dns_txt_record.conde_asuid]
}

output "conde" {
  value = {
    url            = "https://${local.fqdn}"
    azure_fqdn     = azurerm_container_app.server.ingress[0].fqdn
    app            = azurerm_container_app.server.name
    resource_group = azurerm_resource_group.this.name
    key_vault      = azurerm_key_vault.this.name
    postgres       = azurerm_postgresql_flexible_server.this.fqdn
    registry       = "${data.azurerm_container_registry.shared.login_server}/conde"
  }
}
