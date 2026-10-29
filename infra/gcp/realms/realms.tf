# ── Realms: demo and clearscale for the conde workspaces, infra for platform tools ───────────────────────────────
resource "keycloak_realm" "this" {
  for_each = local.realms

  realm        = each.key
  display_name = each.value.display_name
  enabled      = true
  # infra/keycloak/themes: the conde sign-in pages and account console; a password realm's invitation email carries the
  # onboarding steps.
  login_theme   = "conde"
  account_theme = "conde"
  email_theme   = contains(keys(local.password_tenants), each.key) ? "conde" : null

  registration_allowed     = false
  login_with_email_allowed = true
  duplicate_emails_allowed = false
  edit_username_allowed    = false
  remember_me              = false
  verify_email             = true
  reset_password_allowed   = each.value.passwords
  ssl_required             = "external"
  password_policy          = "length(12) and notUsername"

  sso_session_idle_timeout = "30m"
  sso_session_max_lifespan = "10h"

  otp_policy {
    type              = "totp"
    algorithm         = "HmacSHA1"
    digits            = 6
    period            = 30
    look_ahead_window = 1
    code_reusable     = false
  }

  security_defenses {
    brute_force_detection {
      permanent_lockout                = false
      max_login_failures               = 5
      wait_increment_seconds           = 60
      max_failure_wait_seconds         = 900
      failure_reset_time_seconds       = 43200
      quick_login_check_milli_seconds  = 1000
      minimum_quick_login_wait_seconds = 60
    }
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

# Default only where MFA is required; Keycloak assigns a default action to users created after it is set.
resource "keycloak_required_action" "configure_otp" {
  for_each = local.realms

  realm_id       = keycloak_realm.this[each.key].id
  alias          = "CONFIGURE_TOTP"
  name           = "Configure OTP"
  enabled        = true
  default_action = each.value.require_mfa
}

# The workspace in the invitation email (the conde email theme): Keycloak's email templates get the realm's display name,
# not its name, and the subject takes no arguments, so the realm's texts carry them.
resource "keycloak_realm_localization" "invite" {
  for_each = local.password_tenants

  realm_id = keycloak_realm.this[each.key].id
  locale   = "en"
  texts = {
    executeActionsSubject = "You''re invited to ${each.key} on conde"
    condeWorkspace        = each.key
    condeWorkspaceUrl     = "https://${each.value.hostname}"
  }
}

# ── The conde client of each tenant realm; its secret goes to Keycloak and Secret Manager only ────────────────────
ephemeral "random_password" "conde_client_secret" {
  for_each = local.tenants

  length  = 48
  special = false
}

resource "keycloak_openid_client" "conde" {
  for_each = local.tenants

  realm_id    = keycloak_realm.this[each.key].id
  client_id   = "conde"
  name        = "conde"
  access_type = "CONFIDENTIAL"

  standard_flow_enabled        = true
  implicit_flow_enabled        = false
  direct_access_grants_enabled = false
  service_accounts_enabled     = false
  pkce_code_challenge_method   = "S256"

  # The CLI's loopback hop is the server's own redirect after sign-in; Keycloak only ever sees /auth/callback.
  valid_redirect_uris             = ["https://${each.value.hostname}/auth/callback"]
  valid_post_logout_redirect_uris = ["https://${each.value.hostname}/"]
  web_origins                     = ["https://${each.value.hostname}"]
  access_token_lifespan           = "300"

  client_secret_wo         = ephemeral.random_password.conde_client_secret[each.key].result
  client_secret_wo_version = "1"
}

# `basic` carries the sub claim; openid is the protocol scope of every request, not a Keycloak client scope.
resource "keycloak_openid_client_default_scopes" "conde" {
  for_each = local.tenants

  realm_id       = keycloak_realm.this[each.key].id
  client_id      = keycloak_openid_client.conde[each.key].id
  default_scopes = ["basic", "profile", "email"]
}

# The email scope emits it too; this one keeps the claim if that realm default is ever edited.
resource "keycloak_openid_user_property_protocol_mapper" "conde_email_verified" {
  for_each = local.tenants

  realm_id            = keycloak_realm.this[each.key].id
  client_id           = keycloak_openid_client.conde[each.key].id
  name                = "email verified"
  user_property       = "emailVerified"
  claim_name          = "email_verified"
  claim_value_type    = "boolean"
  add_to_id_token     = true
  add_to_access_token = true
  add_to_userinfo     = true
}

# A new version of the secret the workspaces root created; the server mounts versions/latest.
resource "google_secret_manager_secret_version" "conde_client_secret" {
  for_each = local.tenants

  secret                 = "projects/${local.project}/secrets/${each.key}-oidc-client-secret"
  secret_data_wo         = ephemeral.random_password.conde_client_secret[each.key].result
  secret_data_wo_version = 1

  # A recreated client gets a fresh secret in the same apply; the version follows it.
  lifecycle {
    replace_triggered_by = [keycloak_openid_client.conde[each.key].id]
  }
}

# ── Google sign-in: realms whose users sign in only with their company's Google Workspace accounts ─────────────
# The client secret is a reference to Keycloak's file vault, which holds <name>-google-oauth-client-secret (mounted by
# the cluster root), so it is not in this state.
# The default first broker login flow links an existing account only after its password or, once SMTP is set, an emailed link.
resource "keycloak_oidc_google_identity_provider" "this" {
  for_each = local.google_sign_in

  realm       = keycloak_realm.this[each.key].id
  alias       = "google"
  provider_id = "google"
  # The login theme renders "Sign in with <display name>".
  display_name  = "Google"
  enabled       = true
  client_id     = each.value.client_id
  client_secret = "$${vault.google-client-secret}"
  hosted_domain = each.value.hosted_domain
  # Keycloak refuses a token whose hd claim is not the company domain, and Google verifies the address of a Workspace account.
  trust_email        = true
  store_token        = false
  sync_mode          = "IMPORT"
  default_scopes     = "openid email profile"
  hide_on_login_page = false
}

moved {
  from = keycloak_oidc_google_identity_provider.clearscale
  to   = keycloak_oidc_google_identity_provider.this["clearscale"]
}
