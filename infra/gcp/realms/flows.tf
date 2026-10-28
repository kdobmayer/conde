# ── Authentication flows: Google-only sign-in, password plus OTP, and no password grant ─────────────────────────
locals {
  # Aliases without spaces: Cloud Armor refuses percent-encoded paths, and the provider puts the alias in the URL.
  realm_ids = merge({ for name, realm in keycloak_realm.this : name => realm.id }, { master = keycloak_realm.master.id })

  # Password and OTP for every user, enrolled at the first sign-in when missing.
  mfa_realms = ["infra", "master"]
  # Realms without a token-endpoint password grant; Terraform signs in to master with client credentials (README.md).
  no_password_grant = concat(keys(local.google_sign_in), ["infra", "master"])
}

# The browser flow of these realms: an existing session, else straight to Google; no password form.
resource "keycloak_authentication_flow" "google_browser" {
  for_each = local.google_sign_in

  realm_id    = local.realm_ids[each.key]
  alias       = "google-browser"
  description = "Session cookie, else the Google identity provider; the password form is off"
}

# Both ALTERNATIVE, as in Keycloak's browser flow: a REQUIRED step at the same level makes Keycloak ignore the cookie.
resource "keycloak_authentication_execution" "google_cookie" {
  for_each = local.google_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  authenticator     = "auth-cookie"
  requirement       = "ALTERNATIVE"
  priority          = 10
}

resource "keycloak_authentication_execution" "google_redirector" {
  for_each = local.google_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  authenticator     = "identity-provider-redirector"
  requirement       = "ALTERNATIVE"
  priority          = 20
}

resource "keycloak_authentication_execution_config" "google_redirector" {
  for_each = local.google_sign_in

  realm_id     = local.realm_ids[each.key]
  execution_id = keycloak_authentication_execution.google_redirector[each.key].id
  alias        = "google"
  config = {
    defaultProvider = keycloak_oidc_google_identity_provider.this[each.key].alias
  }
}

# Disabled, kept so local passwords can be switched back on: set both requirements (subflow ALTERNATIVE, form REQUIRED).
resource "keycloak_authentication_subflow" "google_forms" {
  for_each = local.google_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.google_browser[each.key].alias
  alias             = "google-browser-forms"
  provider_id       = "basic-flow"
  requirement       = "DISABLED"
  priority          = 30
}

resource "keycloak_authentication_execution" "google_password" {
  for_each = local.google_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_subflow.google_forms[each.key].alias
  authenticator     = "auth-username-password-form"
  requirement       = "DISABLED"
  priority          = 10
}

# The browser flow of the MFA realms: an existing session, else password and OTP, both required.
resource "keycloak_authentication_flow" "mfa_browser" {
  for_each = toset(local.mfa_realms)

  realm_id    = local.realm_ids[each.key]
  alias       = "mfa-browser"
  description = "Session cookie, else password and OTP; a user without OTP enrols at sign-in"
}

# ALTERNATIVE beside the cookie, as in Keycloak's browser flow: a REQUIRED subflow here would make Keycloak ignore it.
resource "keycloak_authentication_execution" "mfa_cookie" {
  for_each = toset(local.mfa_realms)

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.mfa_browser[each.key].alias
  authenticator     = "auth-cookie"
  requirement       = "ALTERNATIVE"
  priority          = 10
}

resource "keycloak_authentication_subflow" "mfa_forms" {
  for_each = toset(local.mfa_realms)

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.mfa_browser[each.key].alias
  alias             = "mfa-browser-forms"
  provider_id       = "basic-flow"
  requirement       = "ALTERNATIVE"
  priority          = 20
}

resource "keycloak_authentication_execution" "mfa_password" {
  for_each = toset(local.mfa_realms)

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_subflow.mfa_forms[each.key].alias
  authenticator     = "auth-username-password-form"
  requirement       = "REQUIRED"
  priority          = 10
}

# REQUIRED with no "user configured" condition: Keycloak sends a user without an OTP credential to Configure OTP.
resource "keycloak_authentication_execution" "mfa_otp" {
  for_each = toset(local.mfa_realms)

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_subflow.mfa_forms[each.key].alias
  authenticator     = "auth-otp-form"
  requirement       = "REQUIRED"
  priority          = 20

  depends_on = [keycloak_authentication_execution.mfa_password]
}

resource "keycloak_authentication_flow" "deny_direct_grant" {
  for_each = toset(local.no_password_grant)

  realm_id    = local.realm_ids[each.key]
  alias       = "deny-direct-grant"
  description = "Refuses every password grant at the token endpoint"
}

resource "keycloak_authentication_execution" "deny_direct_grant" {
  for_each = toset(local.no_password_grant)

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.deny_direct_grant[each.key].alias
  authenticator     = "deny-access-authenticator"
  requirement       = "REQUIRED"
}

# One binding per realm: the provider's bindings resource owns all of a realm's flow bindings.
resource "keycloak_authentication_bindings" "this" {
  for_each = toset(concat(keys(local.google_sign_in), local.mfa_realms))

  realm_id = local.realm_ids[each.key]
  browser_flow = merge(
    { for name, flow in keycloak_authentication_flow.google_browser : name => flow.alias },
    { for name, flow in keycloak_authentication_flow.mfa_browser : name => flow.alias },
  )[each.key]
  direct_grant_flow = try(keycloak_authentication_flow.deny_direct_grant[each.key].alias, null)

  depends_on = [
    keycloak_authentication_execution.google_cookie,
    keycloak_authentication_execution_config.google_redirector,
    keycloak_authentication_execution.google_password,
    keycloak_authentication_execution.mfa_cookie,
    keycloak_authentication_execution.mfa_otp,
    keycloak_authentication_execution.deny_direct_grant,
  ]
}
