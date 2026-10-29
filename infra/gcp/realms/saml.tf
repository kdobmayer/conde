# ── SAML sign-in: realms whose users sign in only through their company's SAML identity provider ─────────────────
# vvpusa: AWS IAM Identity Center. Keycloak is the service provider: entity id https://auth.conde.cloud/realms/<name>,
# assertion consumer URL https://auth.conde.cloud/realms/<name>/broker/<alias>/endpoint. The provider sends the email as
# the NameID and the attributes email, firstName and lastName.
locals {
  saml_idp = {
    for name, saml in local.saml_sign_in : name => {
      issuer      = regex("entityID=\"([^\"]+)\"", saml.metadata)[0]
      sso_url     = regex("SingleSignOnService Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST\" Location=\"([^\"]+)\"", saml.metadata)[0]
      certificate = regex("<ds:X509Certificate>([^<]+)</ds:X509Certificate>", saml.metadata)[0]
    }
  }
}

resource "keycloak_saml_identity_provider" "this" {
  for_each = local.saml_sign_in

  realm        = keycloak_realm.this[each.key].id
  alias        = each.value.alias
  display_name = each.value.display_name
  enabled      = true

  entity_id                  = "${local.keycloak_url}/realms/${each.key}"
  single_sign_on_service_url = local.saml_idp[each.key].sso_url
  # Off: the application knows no logout address of Keycloak's to answer to, so a conde sign-out would end on the
  # provider's page; signing out ends the Keycloak session only.
  single_logout_service_url = ""
  extra_config = {
    idpEntityId = local.saml_idp[each.key].issuer
  }

  name_id_policy_format      = "Email"
  principal_type             = "SUBJECT"
  post_binding_response      = true
  post_binding_authn_request = true
  want_authn_requests_signed = false
  backchannel_supported      = false
  validate_signature         = true
  signing_certificate        = local.saml_idp[each.key].certificate
  want_assertions_signed     = true

  # The company's directory owns the address, as Google Workspace does for clearscale; conde matches invites on it.
  trust_email                   = true
  sync_mode                     = "IMPORT"
  store_token                   = false
  hide_on_login_page            = false
  first_broker_login_flow_alias = keycloak_authentication_flow.saml_first_login[each.key].alias
}

locals {
  saml_attributes = {
    for pair in setproduct(keys(local.saml_sign_in), ["email", "firstName", "lastName"]) :
    "${pair[0]}/${pair[1]}" => { realm = pair[0], attribute = pair[1] }
  }
}

resource "keycloak_attribute_importer_identity_provider_mapper" "saml" {
  for_each = local.saml_attributes

  realm                   = keycloak_realm.this[each.value.realm].id
  name                    = each.value.attribute
  identity_provider_alias = keycloak_saml_identity_provider.this[each.value.realm].alias
  attribute_name          = each.value.attribute
  user_attribute          = each.value.attribute
  extra_config = {
    syncMode = "INHERIT"
  }
}

# The first sign-in creates the account, or links the one with the same email; no review or confirmation pages. The
# realm has no local passwords, so an account with that email can only come from this provider or an admin.
resource "keycloak_authentication_flow" "saml_first_login" {
  for_each = local.saml_sign_in

  realm_id    = local.realm_ids[each.key]
  alias       = "saml-first-login"
  description = "Create the account, else link the one with the same email"
}

resource "keycloak_authentication_execution" "saml_create_user" {
  for_each = local.saml_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.saml_first_login[each.key].alias
  authenticator     = "idp-create-user-if-unique"
  requirement       = "ALTERNATIVE"
  priority          = 10
}

resource "keycloak_authentication_execution" "saml_auto_link" {
  for_each = local.saml_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.saml_first_login[each.key].alias
  authenticator     = "idp-auto-link"
  requirement       = "ALTERNATIVE"
  priority          = 20
}

# The browser flow once the provider is ready (flows.tf binds it): an existing session, else straight to the provider.
resource "keycloak_authentication_flow" "saml_browser" {
  for_each = local.saml_sign_in

  realm_id    = local.realm_ids[each.key]
  alias       = "saml-browser"
  description = "Session cookie, else the SAML identity provider; no password form"
}

# Both ALTERNATIVE, as in Keycloak's browser flow: a REQUIRED step at the same level makes Keycloak ignore the cookie.
resource "keycloak_authentication_execution" "saml_cookie" {
  for_each = local.saml_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.saml_browser[each.key].alias
  authenticator     = "auth-cookie"
  requirement       = "ALTERNATIVE"
  priority          = 10
}

resource "keycloak_authentication_execution" "saml_redirector" {
  for_each = local.saml_sign_in

  realm_id          = local.realm_ids[each.key]
  parent_flow_alias = keycloak_authentication_flow.saml_browser[each.key].alias
  authenticator     = "identity-provider-redirector"
  requirement       = "ALTERNATIVE"
  priority          = 20
}

resource "keycloak_authentication_execution_config" "saml_redirector" {
  for_each = local.saml_sign_in

  realm_id     = local.realm_ids[each.key]
  execution_id = keycloak_authentication_execution.saml_redirector[each.key].id
  alias        = "saml-${each.value.alias}"
  config = {
    defaultProvider = keycloak_saml_identity_provider.this[each.key].alias
  }
}
