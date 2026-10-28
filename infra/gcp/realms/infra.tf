# ── Realm infra: Argo CD sign-in for the infra-admins group, MFA required ─────────────────────────────────────────
locals {
  infra_realm = keycloak_realm.this["infra"].id
  argocd_url  = "https://argocd.conde.cloud"
}

resource "keycloak_group" "infra_admins" {
  realm_id = local.infra_realm
  name     = "infra-admins"
}

resource "keycloak_openid_client_scope" "groups" {
  realm_id               = local.infra_realm
  name                   = "groups"
  description            = "Group membership as the groups claim"
  include_in_token_scope = true
}

resource "keycloak_openid_group_membership_protocol_mapper" "groups" {
  realm_id            = local.infra_realm
  client_scope_id     = keycloak_openid_client_scope.groups.id
  name                = "groups"
  claim_name          = "groups"
  full_path           = false
  add_to_id_token     = true
  add_to_access_token = true
  add_to_userinfo     = true
}

# Public with PKCE: Argo CD's UI redeems the code in the browser, so its origin is a web origin.
resource "keycloak_openid_client" "argocd" {
  realm_id    = local.infra_realm
  client_id   = "argocd"
  name        = "Argo CD"
  access_type = "PUBLIC"

  standard_flow_enabled        = true
  implicit_flow_enabled        = false
  direct_access_grants_enabled = false
  pkce_code_challenge_method   = "S256"

  valid_redirect_uris = ["${local.argocd_url}/auth/callback", "${local.argocd_url}/pkce/verify", "http://localhost:8085/auth/callback"]
  web_origins         = [local.argocd_url]
}

resource "keycloak_openid_client_default_scopes" "argocd" {
  realm_id       = local.infra_realm
  client_id      = keycloak_openid_client.argocd.id
  default_scopes = ["basic", "profile", "email", keycloak_openid_client_scope.groups.name]
}

ephemeral "random_password" "krisztian" {
  length  = 24
  special = false
}

resource "keycloak_user" "krisztian" {
  realm_id       = local.infra_realm
  username       = "krisztian"
  email          = local.owner_email
  email_verified = true
  enabled        = true

  required_actions = ["UPDATE_PASSWORD", "CONFIGURE_TOTP"]

  initial_password {
    value_wo         = ephemeral.random_password.krisztian.result
    value_wo_version = "1"
    temporary        = true
  }

  # Keycloak clears the actions as he completes them; Terraform does not put them back.
  lifecycle {
    ignore_changes = [required_actions]
  }
}

resource "keycloak_user_groups" "krisztian" {
  realm_id  = local.infra_realm
  user_id   = keycloak_user.krisztian.id
  group_ids = [keycloak_group.infra_admins.id]
}

resource "google_secret_manager_secret_version" "krisztian" {
  secret                 = google_secret_manager_secret.generated["infra-krisztian-temp-password"].id
  secret_data_wo         = ephemeral.random_password.krisztian.result
  secret_data_wo_version = 1

  lifecycle {
    replace_triggered_by = [keycloak_user.krisztian.id]
  }
}
