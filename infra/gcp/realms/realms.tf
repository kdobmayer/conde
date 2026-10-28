# ── Realms: demo and clearscale for the conde workspaces, infra for platform tools ───────────────────────────────
resource "keycloak_realm" "this" {
  for_each = local.realms

  realm        = each.key
  display_name = each.value.display_name
  enabled      = true

  registration_allowed     = false
  login_with_email_allowed = true
  duplicate_emails_allowed = false
  edit_username_allowed    = false
  remember_me              = false
  verify_email             = true
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

  dynamic "smtp_server" {
    for_each = var.smtp == null ? [] : [var.smtp]
    content {
      host              = smtp_server.value.host
      port              = smtp_server.value.port
      from              = smtp_server.value.from
      from_display_name = each.value.display_name
      starttls          = true

      auth {
        username = smtp_server.value.username
        password = var.smtp_password
      }
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
# The provider has no write-only client secret, so this value is in this state, like the SMTP password.
data "google_secret_manager_secret_version" "google_sign_in" {
  for_each = local.google_sign_in

  secret = "${each.key}-google-oauth-client-secret"
}

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
  client_secret = data.google_secret_manager_secret_version.google_sign_in[each.key].secret_data
  hosted_domain = each.value.hosted_domain
  # Keycloak refuses a token whose hd claim is not the company domain, and Google verifies the address of a Workspace account.
  trust_email        = true
  store_token        = false
  sync_mode          = "IMPORT"
  default_scopes     = "openid email profile"
  hide_on_login_page = false
}

moved {
  from = data.google_secret_manager_secret_version.clearscale_google
  to   = data.google_secret_manager_secret_version.google_sign_in["clearscale"]
}

moved {
  from = keycloak_oidc_google_identity_provider.clearscale
  to   = keycloak_oidc_google_identity_provider.this["clearscale"]
}

# The browser flow of these realms: an existing session, else straight to Google; no password form.
resource "keycloak_authentication_flow" "google_browser" {
  for_each = local.google_sign_in

  realm_id    = keycloak_realm.this[each.key].id
  alias       = "google browser"
  description = "Session cookie, else the Google identity provider; the password form is off"
}

# Both ALTERNATIVE, as in Keycloak's browser flow: a REQUIRED step at the same level makes Keycloak ignore the cookie.
resource "keycloak_authentication_execution" "google_cookie" {
  for_each = local.google_sign_in

  realm_id          = keycloak_realm.this[each.key].id
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  authenticator     = "auth-cookie"
  requirement       = "ALTERNATIVE"
  priority          = 10
}

resource "keycloak_authentication_execution" "google_redirector" {
  for_each = local.google_sign_in

  realm_id          = keycloak_realm.this[each.key].id
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  authenticator     = "identity-provider-redirector"
  requirement       = "ALTERNATIVE"
  priority          = 20
}

resource "keycloak_authentication_execution_config" "google_redirector" {
  for_each = local.google_sign_in

  realm_id     = keycloak_realm.this[each.key].id
  execution_id = keycloak_authentication_execution.google_redirector[each.key].id
  alias        = "google"
  config = {
    defaultProvider = keycloak_oidc_google_identity_provider.this[each.key].alias
  }
}

# Disabled, kept so local passwords can be switched back on: set both requirements (subflow ALTERNATIVE, form REQUIRED).
resource "keycloak_authentication_subflow" "google_forms" {
  for_each = local.google_sign_in

  realm_id          = keycloak_realm.this[each.key].id
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  alias             = "google browser forms"
  provider_id       = "basic-flow"
  requirement       = "DISABLED"
  priority          = 30
}

resource "keycloak_authentication_execution" "google_password" {
  for_each = local.google_sign_in

  realm_id          = keycloak_realm.this[each.key].id
  parent_flow_alias = keycloak_authentication_subflow.google_forms[each.key].alias
  authenticator     = "auth-username-password-form"
  requirement       = "DISABLED"
  priority          = 10
}

resource "keycloak_authentication_bindings" "google" {
  for_each = local.google_sign_in

  realm_id     = keycloak_realm.this[each.key].id
  browser_flow = keycloak_authentication_flow.google_browser[each.key].alias

  depends_on = [
    keycloak_authentication_execution.google_cookie,
    keycloak_authentication_execution_config.google_redirector,
    keycloak_authentication_execution.google_password,
  ]
}
