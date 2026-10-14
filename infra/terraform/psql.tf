# The read-only query path to the board's Postgres (private endpoint, no VPN): a manual Container Apps Job on the
# official postgres image with the vault's connection string. Its own command only proves the connection; a real
# query is passed at start time as a container override (README), never baked into the job. Same database user as
# the server: a separate reader role would need its own password and URL in the vault for a path used a few times a
# month, so it stays with the one user and the discipline of SELECT-only scripts.
resource "azurerm_container_app_job" "psql" {
  name                         = "caj-${local.name}-psql"
  location                     = azurerm_resource_group.this.location
  resource_group_name          = azurerm_resource_group.this.name
  container_app_environment_id = azurerm_container_app_environment.this.id
  workload_profile_name        = "Consumption"
  replica_timeout_in_seconds   = 600
  replica_retry_limit          = 0
  tags                         = local.tags

  manual_trigger_config {
    parallelism              = 1
    replica_completion_count = 1
  }

  identity {
    type         = "UserAssigned"
    identity_ids = [azurerm_user_assigned_identity.app.id]
  }

  secret {
    name                = azurerm_key_vault_secret.database_url.name
    key_vault_secret_id = azurerm_key_vault_secret.database_url.versionless_id
    identity            = azurerm_user_assigned_identity.app.id
  }

  template {
    container {
      name    = "psql"
      image   = "docker.io/library/postgres:18-alpine" # Debian postgres images carry no CA bundle for verify-full
      cpu     = 0.5
      memory  = "1Gi"
      command = ["sh", "-c", "psql \"$DATABASE_URL\" -v ON_ERROR_STOP=1 -c 'select now(), current_user'"]

      env {
        name        = "DATABASE_URL"
        secret_name = azurerm_key_vault_secret.database_url.name
      }
      env {
        name  = "PGCONNECT_TIMEOUT"
        value = "20"
      }
    }
  }

  depends_on = [time_sleep.rbac]
}
