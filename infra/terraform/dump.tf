# Temporary pg_dump of the board's database for the move to GCP (docs/CUTOVER-dakai.md): a Container Apps Job on the
# psql image writes a custom-format dump onto a writable SMB share, which the owner downloads with his Entra login.
# Read only on the database (pg_dump runs in one repeatable-read snapshot). `dump_enabled = false` removes the job, the
# share and its storage account. A separate account for the same reason as import.tf: SMB mounts need the account key.
variable "dump_enabled" {
  type    = bool
  default = false
}

resource "azurerm_storage_account" "dump" {
  count                           = var.dump_enabled ? 1 : 0
  name                            = "stcondedump"
  location                        = azurerm_resource_group.this.location
  resource_group_name             = azurerm_resource_group.this.name
  account_tier                    = "Standard"
  account_replication_type        = "LRS"
  account_kind                    = "StorageV2"
  min_tls_version                 = "TLS1_2"
  allow_nested_items_to_be_public = false
  tags                            = local.tags
}

resource "azurerm_storage_share" "dump" {
  count              = var.dump_enabled ? 1 : 0
  name               = "conde-dump"
  storage_account_id = azurerm_storage_account.dump[0].id
  quota              = 5
}

# The owner downloads with `az storage file download --auth-mode login` (no key on his side).
resource "azurerm_role_assignment" "dump_download" {
  count                = var.dump_enabled ? 1 : 0
  scope                = azurerm_storage_account.dump[0].id
  role_definition_name = "Storage File Data Privileged Contributor"
  principal_id         = local.owner_object_id
  principal_type       = "User"
}

resource "azurerm_container_app_environment_storage" "dump" {
  count                        = var.dump_enabled ? 1 : 0
  name                         = "conde-dump"
  container_app_environment_id = azurerm_container_app_environment.this.id
  account_name                 = azurerm_storage_account.dump[0].name
  access_key                   = azurerm_storage_account.dump[0].primary_access_key
  share_name                   = azurerm_storage_share.dump[0].name
  access_mode                  = "ReadWrite"
}

resource "azurerm_container_app_job" "dump" {
  count                        = var.dump_enabled ? 1 : 0
  name                         = "caj-${local.name}-dump"
  location                     = azurerm_resource_group.this.location
  resource_group_name          = azurerm_resource_group.this.name
  container_app_environment_id = azurerm_container_app_environment.this.id
  workload_profile_name        = "Consumption"
  replica_timeout_in_seconds   = 1800
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
      name   = "dump"
      image  = "docker.io/library/postgres:18-alpine"
      cpu    = 0.5
      memory = "1Gi"
      # One file per run, named by its UTC start; written under .part and renamed, so a listed .dump is complete. A
      # psql session holds a repeatable-read snapshot that pg_dump shares, and counts.sql runs in it after the dump, so
      # the .counts file describes exactly the dumped data even while the server keeps writing.
      command = ["sh", "-c", <<-EOT
        set -eu
        f=/mnt/dump/conde-$(date -u +%Y%m%dT%H%M%SZ).dump
        mkfifo /tmp/q
        psql "$DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 </tmp/q >/tmp/r 2>&1 &
        p=$!
        exec 3>/tmp/q
        printf '%s\n' 'BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY;' 'SELECT pg_export_snapshot();' >&3
        i=0
        until s=$(grep -m1 -E '^[0-9A-F]+-[0-9A-F]+-[0-9]+$' /tmp/r); do
          i=$((i + 1)); [ "$i" -lt 60 ] || { cat /tmp/r; exit 1; }; sleep 1
        done
        echo "snapshot $s"
        pg_dump "$DATABASE_URL" --snapshot="$s" -Fc --no-owner --no-acl -f "$f.part"
        printf '%s\n' "$COUNTS_SQL" 'COMMIT;' >&3
        exec 3>&-
        wait "$p"
        grep -v -x "$s" /tmp/r | tee "$f.counts"
        mv "$f.part" "$f"
        sha256sum "$f" | tee "$f.sha256"
        ls -l "$f"
      EOT
      ]

      env {
        name        = "DATABASE_URL"
        secret_name = azurerm_key_vault_secret.database_url.name
      }
      env {
        name  = "COUNTS_SQL"
        value = file("${path.module}/../cutover/counts.sql")
      }
      env {
        name  = "PGCONNECT_TIMEOUT"
        value = "20"
      }

      volume_mounts {
        name = "dump"
        path = "/mnt/dump"
      }
    }

    volume {
      name         = "dump"
      storage_type = "AzureFile"
      storage_name = azurerm_container_app_environment_storage.dump[0].name
    }
  }

  depends_on = [time_sleep.rbac]
}
