# ── Cloud SQL for PostgreSQL: PSC only (no public IP, no private services access), IAM database auth on ─────────
resource "google_sql_database_instance" "pg" {
  name                = "conde-pg"
  region              = local.region
  database_version    = "POSTGRES_18"
  deletion_protection = true

  root_password_wo         = ephemeral.google_secret_manager_secret_version.pg_admin.secret_data
  root_password_wo_version = var.pg_admin_password_version

  settings {
    edition                     = "ENTERPRISE"
    tier                        = "db-custom-1-3840"
    availability_type           = "ZONAL"
    disk_type                   = "PD_SSD"
    disk_size                   = 20
    disk_autoresize             = true
    deletion_protection_enabled = true

    database_flags {
      name  = "cloudsql.iam_authentication"
      value = "on"
    }

    backup_configuration {
      enabled                        = true
      point_in_time_recovery_enabled = true
      transaction_log_retention_days = 7

      backup_retention_settings {
        retained_backups = 7
      }
    }

    # Sunday (7), 02:00 UTC.
    maintenance_window {
      day  = 7
      hour = 2
    }

    ip_configuration {
      ipv4_enabled = false
      ssl_mode     = "ENCRYPTED_ONLY"

      psc_config {
        psc_enabled               = true
        allowed_consumer_projects = [local.project]
      }
    }
  }
}

locals {
  pg_fqdn = "${trimsuffix(google_sql_database_instance.pg.dns_name, ".")}."
}

# The endpoint in our VPC that the Auth Proxy reaches through the instance's DNS name.
resource "google_compute_forwarding_rule" "pg_psc" {
  name                  = "psc-conde-pg"
  region                = local.region
  network               = local.foundation.network_id
  ip_address            = local.foundation.psc_address_self_link
  load_balancing_scheme = ""
  target                = google_sql_database_instance.pg.psc_service_attachment_link
}

resource "google_dns_managed_zone" "pg_psc" {
  name        = "conde-pg-psc"
  dns_name    = local.pg_fqdn
  description = "Private resolution of the Cloud SQL instance conde-pg to its PSC endpoint"
  visibility  = "private"

  private_visibility_config {
    networks {
      network_url = local.foundation.network_id
    }
  }
}

resource "google_dns_record_set" "pg_psc" {
  managed_zone = google_dns_managed_zone.pg_psc.name
  name         = local.pg_fqdn
  type         = "A"
  ttl          = 300
  rrdatas      = [local.foundation.psc_address]
}

resource "google_sql_database" "keycloak" {
  name     = "keycloak"
  instance = google_sql_database_instance.pg.name
}

resource "google_sql_user" "keycloak" {
  name                = "keycloak"
  instance            = google_sql_database_instance.pg.name
  password_wo         = ephemeral.google_secret_manager_secret_version.keycloak_db.secret_data
  password_wo_version = var.keycloak_db_password_version
}
