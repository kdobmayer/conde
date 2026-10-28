# ── Master realm: the permanent human admin, and the client Terraform signs in with once bootstrap-admin is gone ──
data "keycloak_role" "master_admin" {
  realm_id = "master"
  name     = "admin"
}

ephemeral "random_password" "admin" {
  length  = 24
  special = false
}

resource "keycloak_user" "admin" {
  realm_id       = "master"
  username       = "admin"
  email          = local.owner_email
  email_verified = true
  enabled        = true

  required_actions = ["UPDATE_PASSWORD", "CONFIGURE_TOTP"]

  initial_password {
    value_wo         = ephemeral.random_password.admin.result
    value_wo_version = "1"
    temporary        = true
  }

  # Keycloak clears the actions as he completes them; Terraform does not put them back.
  lifecycle {
    ignore_changes = [required_actions]
  }
}

# Not exhaustive: the user keeps the realm's default roles.
resource "keycloak_user_roles" "admin" {
  realm_id   = "master"
  user_id    = keycloak_user.admin.id
  role_ids   = [data.keycloak_role.master_admin.id]
  exhaustive = false
}

resource "google_secret_manager_secret_version" "admin" {
  secret                 = google_secret_manager_secret.generated["keycloak-admin-temp-password"].id
  secret_data_wo         = ephemeral.random_password.admin.result
  secret_data_wo_version = 1

  lifecycle {
    replace_triggered_by = [keycloak_user.admin.id]
  }
}

ephemeral "random_password" "terraform_client_secret" {
  length  = 48
  special = false
}

# Client credentials only: a password grant cannot answer the OTP the human admins enrol.
resource "keycloak_openid_client" "terraform" {
  realm_id    = "master"
  client_id   = "terraform"
  name        = "Terraform (infra/gcp/realms)"
  access_type = "CONFIDENTIAL"

  standard_flow_enabled        = false
  implicit_flow_enabled        = false
  direct_access_grants_enabled = false
  service_accounts_enabled     = true

  client_secret_wo         = ephemeral.random_password.terraform_client_secret.result
  client_secret_wo_version = tostring(var.terraform_client_secret_version)
}

resource "keycloak_openid_client_service_account_realm_role" "terraform" {
  realm_id                = "master"
  service_account_user_id = keycloak_openid_client.terraform.service_account_user_id
  role                    = data.keycloak_role.master_admin.name
}

# A rotation disables the version holding the old value instead of destroying it.
resource "google_secret_manager_secret_version" "terraform_client_secret" {
  secret                 = google_secret_manager_secret.generated["keycloak-terraform-client-secret"].id
  secret_data_wo         = ephemeral.random_password.terraform_client_secret.result
  secret_data_wo_version = var.terraform_client_secret_version
  deletion_policy        = "DISABLE"

  lifecycle {
    replace_triggered_by = [keycloak_openid_client.terraform.id]
  }
}

# Managed for its frontend URL, its display name, its themes and its outbound mail (admin password resets and the console's test
# email); other settings stay Keycloak's. The frontend URL moves the master realm's sign-in, and with it the console, to
# the host behind IAP.
import {
  to = keycloak_realm.master
  id = "master"
}

resource "keycloak_realm" "master" {
  realm                       = "master"
  default_signature_algorithm = "RS256"
  # The kicker under the wordmark on the admin sign-in page; Keycloak's default HTML name would show KEYCLOAK.
  display_name      = "conde admin"
  display_name_html = "conde admin"
  # The admin console's sign-in page and the admins' account console in the conde theme (infra/keycloak/themes).
  login_theme   = "conde"
  account_theme = "conde"

  attributes = {
    frontendUrl = local.master_url
  }

  smtp_server {
    host              = local.smtp.host
    port              = local.smtp.port
    ssl               = true
    from              = local.smtp.from
    from_display_name = local.smtp.from_display_name
    reply_to          = local.smtp.reply_to

    auth {
      username = local.smtp.username
      password = local.smtp.password
    }
  }

  terraform_deletion_protection = true
}
