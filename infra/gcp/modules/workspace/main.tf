# One conde workspace, GCP side: its database and IAM database user on conde-pg, its Google service account and the
# Workload Identity grants of its Kubernetes service accounts, and its secrets. The Kubernetes objects that serve
# https://<hostname> are Argo CD's (charts/conde-workspace).
locals {
  project      = var.platform.project
  sql_instance = split(":", var.platform.sql_connection_name)[2]
  namespace    = "ws-${var.name}"
  ksa          = "conde"
  dbinit_ksa   = "dbinit"
  # Cloud SQL names an IAM service-account user by its email without the .gserviceaccount.com suffix.
  db_user           = trimsuffix(google_service_account.this.email, ".gserviceaccount.com")
  oidc_issuer       = coalesce(var.oidc_issuer, "${var.platform.keycloak_url}/realms/${var.name}")
  oidc_redirect_uri = "https://${var.hostname}/auth/callback"
  wi_pool           = "${local.project}.svc.id.goog"
  # The dbinit KSA has no GSA annotation: it is granted as its own Workload Identity principal.
  dbinit_principal = "principal://iam.googleapis.com/projects/${var.platform.project_number}/locations/global/workloadIdentityPools/${local.wi_pool}/subject/ns/${local.namespace}/sa/${local.dbinit_ksa}"
  app_secrets = {
    "admin-key"          = google_secret_manager_secret.admin_key.secret_id
    "oidc-client-secret" = google_secret_manager_secret.manual["oidc-client-secret"].secret_id
    "slack-bot-token"    = google_secret_manager_secret.manual["slack-bot-token"].secret_id
  }
}

# ── Database: conde_<name>, owned by the workspace's IAM user (granted by the dbinit Job) ───────────────────────
resource "google_sql_database" "this" {
  name     = "conde_${var.name}"
  instance = local.sql_instance
}

resource "google_service_account" "this" {
  account_id   = "conde-${var.name}"
  display_name = "conde workspace ${var.name}"
}

resource "google_project_iam_member" "sql" {
  for_each = toset(["roles/cloudsql.client", "roles/cloudsql.instanceUser"])

  project = local.project
  role    = each.value
  member  = google_service_account.this.member
}

resource "google_sql_user" "this" {
  name     = local.db_user
  instance = local.sql_instance
  type     = "CLOUD_IAM_SERVICE_ACCOUNT"
}

resource "google_service_account_iam_member" "workload_identity" {
  service_account_id = google_service_account.this.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${local.wi_pool}[${local.namespace}/${local.ksa}]"
}

# The dbinit Job's Auth Proxy connects with a password, so the principal needs only Cloud SQL Client.
resource "google_project_iam_member" "dbinit_sql" {
  project = local.project
  role    = "roles/cloudsql.client"
  member  = local.dbinit_principal
}

# ── Secrets: the admin key is generated here; the OIDC client secret and Slack bot token are set by hand ─────────
ephemeral "random_password" "admin_key" {
  length  = 48
  special = false
}

resource "google_secret_manager_secret" "admin_key" {
  secret_id = "${var.name}-admin-key"

  replication {
    user_managed {
      replicas {
        location = var.platform.region
      }
    }
  }
}

# Write-only: the key is in Secret Manager only, never in this state.
resource "google_secret_manager_secret_version" "admin_key" {
  secret                 = google_secret_manager_secret.admin_key.id
  secret_data_wo         = ephemeral.random_password.admin_key.result
  secret_data_wo_version = 1
}

# No version until one is added by hand; the pods wait in ContainerCreating until then (../../workspaces/README.md).
resource "google_secret_manager_secret" "manual" {
  for_each = toset(["oidc-client-secret", "slack-bot-token"])

  secret_id = "${var.name}-${each.value}"

  replication {
    user_managed {
      replicas {
        location = var.platform.region
      }
    }
  }
}

resource "google_secret_manager_secret_iam_member" "app" {
  for_each = local.app_secrets

  project   = local.project
  secret_id = "projects/${local.project}/secrets/${each.value}"
  role      = "roles/secretmanager.secretAccessor"
  # The CSI add-on reads as the Google service account the pod's KSA is annotated with.
  member = google_service_account.this.member
}

resource "google_secret_manager_secret_iam_member" "dbinit" {
  project   = local.project
  secret_id = "projects/${local.project}/secrets/${var.platform.pg_admin_secret}"
  role      = "roles/secretmanager.secretAccessor"
  member    = local.dbinit_principal
}
