# Temporary pg_dump of the board's database for the move to GCP (docs/CUTOVER-dakai.md): a Container Apps Job on the
# psql image writes a custom-format dump onto a writable SMB share, which the owner downloads with his Entra login.
# Read only on the database (pg_dump runs in one repeatable-read snapshot). The restore job below is the way back
# (docs/CUTOVER-dakai-azure.md). `dump_enabled = false` removes both jobs, the share and its storage account. A
# separate account for the same reason as import.tf: SMB mounts need the account key.
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

# The reverse direction for the move back from GKE (docs/CUTOVER-dakai-azure.md): replaces the content of the board's
# database with a dump uploaded to the same share. The request is the dump's exact basename and expected sha256, as
# DUMP and DUMP_SHA256 in the start's execution template; without them the job reads `<basename> <sha256>` from
# restore.request on the share. It refuses a checksum mismatch and a dump it already restored.
resource "azurerm_container_app_job" "restore" {
  count                        = var.dump_enabled ? 1 : 0
  name                         = "caj-${local.name}-restore"
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
      name = "restore"
      # The chart's dbinit image (charts/conde-workspace/values.yaml), the client the GKE dump Job uses.
      image  = "docker.io/library/postgres:18.6-alpine@sha256:10a5c69a83035ab5239f5620d708520e8b28f5aa1f2b20143e6024532edadfb5"
      cpu    = 0.5
      memory = "1Gi"
      # Everything is checked before the schema drop. The drop runs outside pg_restore's single transaction, so a
      # failed restore leaves an empty public schema; the dump that was replaced stays on this share.
      command = ["sh", "-c", <<-EOT
        set -eu
        echo "env: $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort | tr '\n' ' ')"
        : "$DATABASE_URL" "$COUNTS_SQL"
        cd /mnt/dump
        if [ -z "$${DUMP:-}" ]; then
          read -r DUMP DUMP_SHA256 <restore.request
        fi
        case "$DUMP" in
          */* | .* | *[!A-Za-z0-9._-]* | '') echo "refused: bad dump name '$DUMP'"; exit 1 ;;
          *.dump) ;;
          *) echo "refused: '$DUMP' is not a .dump"; exit 1 ;;
        esac
        out="$DUMP.restore.counts"
        [ ! -e "$out" ] || { echo "refused: $out exists, $DUMP was restored already"; exit 1; }
        [ -f "$DUMP" ] || { echo "refused: $DUMP is not on the share"; exit 1; }
        got=$(sha256sum "$DUMP" | cut -d' ' -f1)
        [ "$got" = "$DUMP_SHA256" ] || { echo "refused: sha256 $got, expected $DUMP_SHA256"; exit 1; }
        echo "checksum ok $DUMP $got"
        : >"$out.part"
        pg_restore --version
        psql --version
        psql "$DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 \
          -c "SELECT 'server_version|' || current_setting('server_version')" \
          -c "SELECT 'extension|' || extname || '|' || extversion FROM pg_extension ORDER BY extname"
        echo "archive entries outside schema public:"
        pg_restore -l "$DUMP" | grep -v -e '^;' -e ' SCHEMA - public ' | grep -v -E '^[0-9]+; [0-9]+ [0-9]+ [A-Z ]+ public ' || true
        s=$(date +%s)
        psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -c 'DROP SCHEMA public CASCADE' -c 'CREATE SCHEMA public'
        pg_restore -d "$DATABASE_URL" --no-owner --no-acl --exit-on-error --single-transaction "$DUMP"
        echo "restore seconds $(($(date +%s) - s))"
        psql "$DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 \
          -c "SELECT 'OWNER tables:' || tableowner, count(*) FROM pg_tables WHERE schemaname = 'public' GROUP BY 1" \
          -c "SELECT 'OWNER relkind ' || c.relkind::text || ':' || pg_get_userbyid(c.relowner), count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' GROUP BY 1 ORDER BY 1" \
          -c "SELECT 'OWNER schema:' || pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'public'" \
          -c "SELECT 'OWNER database:' || pg_get_userbyid(datdba) FROM pg_database WHERE datname = current_database()"
        printf '%s\n' "$COUNTS_SQL" | psql "$DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 >"$out.part"
        mv "$out.part" "$out"
        sed 's/^/COUNT /' "$out"
        echo "restore done $DUMP"
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
