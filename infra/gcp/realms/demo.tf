# ── demo: the admin API client conde's server invites users with (its user profile is in realms.tf) ──────────
# Through the conde-admin client, not the realm: a pending realm change would defer this read to apply and replace the
# role mapping below (invites fail until it is back).
data "keycloak_openid_client" "demo_realm_management" {
  realm_id  = keycloak_openid_client.conde_admin_demo.realm_id
  client_id = "realm-management"
}

ephemeral "random_password" "conde_admin_demo" {
  length  = 48
  special = false
}

# Client credentials only; its service account may manage the realm's users and nothing else.
resource "keycloak_openid_client" "conde_admin_demo" {
  realm_id    = keycloak_realm.this["demo"].id
  client_id   = "conde-admin"
  name        = "conde admin API"
  access_type = "CONFIDENTIAL"

  standard_flow_enabled        = false
  implicit_flow_enabled        = false
  direct_access_grants_enabled = false
  service_accounts_enabled     = true

  client_secret_wo         = ephemeral.random_password.conde_admin_demo.result
  client_secret_wo_version = "1"
}

resource "keycloak_openid_client_service_account_role" "conde_admin_demo" {
  realm_id                = keycloak_realm.this["demo"].id
  service_account_user_id = keycloak_openid_client.conde_admin_demo.service_account_user_id
  client_id               = data.keycloak_openid_client.demo_realm_management.id
  role                    = "manage-users"
}

resource "google_secret_manager_secret_version" "conde_admin_demo" {
  secret                 = google_secret_manager_secret.generated["keycloak-admin-demo"].id
  secret_data_wo         = ephemeral.random_password.conde_admin_demo.result
  secret_data_wo_version = 1

  lifecycle {
    replace_triggered_by = [keycloak_openid_client.conde_admin_demo.id]
  }
}
