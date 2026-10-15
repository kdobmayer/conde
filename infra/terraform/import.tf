# One-off import of the kond board into the cloud Postgres: a Container Apps Job on the same image with the kond
# snapshot on an SMB file share mounted read-only at /mnt/kond. Temporary: `import_enabled = false` removes the job,
# the share and its storage account once the cutover is done. The share's account key is what Container Apps needs
# to mount SMB, so this account is separate from everything else, holds only the snapshot, and dies with the import.
variable "import_enabled" {
  type    = bool
  default = false
}

resource "azurerm_storage_account" "import" {
  count                           = var.import_enabled ? 1 : 0
  name                            = "stcondeimport"
  location                        = azurerm_resource_group.this.location
  resource_group_name             = azurerm_resource_group.this.name
  account_tier                    = "Standard"
  account_replication_type        = "LRS"
  account_kind                    = "StorageV2"
  min_tls_version                 = "TLS1_2"
  allow_nested_items_to_be_public = false
  tags                            = local.tags
}

resource "azurerm_storage_share" "kond_snapshot" {
  count              = var.import_enabled ? 1 : 0
  name               = "kond-snapshot"
  storage_account_id = azurerm_storage_account.import[0].id
  quota              = 5
}

# The owner uploads the snapshot with `az storage file upload-batch --auth-mode login` (no key on his side).
resource "azurerm_role_assignment" "import_upload" {
  count                = var.import_enabled ? 1 : 0
  scope                = azurerm_storage_account.import[0].id
  role_definition_name = "Storage File Data Privileged Contributor"
  principal_id         = local.owner_object_id
  principal_type       = "User"
}

resource "azurerm_container_app_environment_storage" "kond_snapshot" {
  count                        = var.import_enabled ? 1 : 0
  name                         = "kond-snapshot"
  container_app_environment_id = azurerm_container_app_environment.this.id
  account_name                 = azurerm_storage_account.import[0].name
  access_key                   = azurerm_storage_account.import[0].primary_access_key
  share_name                   = azurerm_storage_share.kond_snapshot[0].name
  access_mode                  = "ReadOnly"
}

resource "azurerm_container_app_job" "import" {
  count                        = var.import_enabled ? 1 : 0
  name                         = "caj-${local.name}-import"
  location                     = azurerm_resource_group.this.location
  resource_group_name          = azurerm_resource_group.this.name
  container_app_environment_id = azurerm_container_app_environment.this.id
  workload_profile_name        = "Consumption"
  replica_timeout_in_seconds   = 7200
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

  registry {
    server   = data.azurerm_container_registry.shared.login_server
    identity = azurerm_user_assigned_identity.app.id
  }

  secret {
    name                = azurerm_key_vault_secret.database_url.name
    key_vault_secret_id = azurerm_key_vault_secret.database_url.versionless_id
    identity            = azurerm_user_assigned_identity.app.id
  }

  template {
    container {
      name   = "import"
      image  = var.image
      cpu    = 1
      memory = "2Gi"
      # Dry run by default; the real run overrides the args at start time (README).
      args = ["import", "--dry-run", "--from", "/mnt/kond", "--owner", "kdobmayer", "--host", "homepc"]

      env {
        name        = "DATABASE_URL"
        secret_name = azurerm_key_vault_secret.database_url.name
      }
      env {
        name  = "RUST_LOG"
        value = "info"
      }

      volume_mounts {
        name = "kond"
        path = "/mnt/kond"
      }
    }

    volume {
      name         = "kond"
      storage_type = "AzureFile"
      storage_name = azurerm_container_app_environment_storage.kond_snapshot[0].name
    }
  }

  depends_on = [time_sleep.rbac]
}
