# ── Generated passwords: written write-only to Secret Manager, read back ephemerally; none lands in state ──────
# The read-back keeps the database equal to the secret even when an apply fails between the two writes.
ephemeral "random_password" "pg_admin" {
  length  = 32
  special = false
}

ephemeral "random_password" "keycloak_db" {
  length  = 32
  special = false
}

resource "google_secret_manager_secret" "pg_admin" {
  secret_id = "pg-admin-password"

  replication {
    user_managed {
      replicas {
        location = local.region
      }
    }
  }
}

resource "google_secret_manager_secret_version" "pg_admin" {
  secret                 = google_secret_manager_secret.pg_admin.id
  secret_data_wo         = ephemeral.random_password.pg_admin.result
  secret_data_wo_version = var.pg_admin_password_version
}

# version is unknown exactly when the secret version is created or rotated, which defers this read to the apply.
ephemeral "google_secret_manager_secret_version" "pg_admin" {
  secret  = google_secret_manager_secret_version.pg_admin.secret
  version = google_secret_manager_secret_version.pg_admin.version
}

resource "google_secret_manager_secret" "keycloak_db" {
  secret_id = "keycloak-db-password"

  replication {
    user_managed {
      replicas {
        location = local.region
      }
    }
  }
}

resource "google_secret_manager_secret_version" "keycloak_db" {
  secret                 = google_secret_manager_secret.keycloak_db.id
  secret_data_wo         = ephemeral.random_password.keycloak_db.result
  secret_data_wo_version = var.keycloak_db_password_version
}

ephemeral "google_secret_manager_secret_version" "keycloak_db" {
  secret  = google_secret_manager_secret_version.keycloak_db.secret
  version = google_secret_manager_secret_version.keycloak_db.version
}
