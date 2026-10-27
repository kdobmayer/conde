# Azure SSO for conde's human users: one single-tenant app registration (OpenID Connect, authorization code +
# PKCE, confidential client), its enterprise app requires assignment, and only the listed users are assigned.
# Agents keep API keys; this is for people. The client secret is created by hand (az ad app credential reset) and
# written straight into the vault as entra-client-secret, so it never enters this state (README).
locals {
  sso_users = {
    krisztian  = "da79aae8-c6e1-4f10-9826-42aeeab0c371" # krisztian.dobmayer@dakai.io
    david      = "000cfce6-748a-4f2d-b922-eec1643e31f6" # david.pacsuta@dakai.io
    balazs     = "a66cc326-8ce1-4271-9c47-16ba30b2c541" # balazs.rozsenich@dakai.io
    daniel     = "c65cde33-e8f5-4004-aaf3-b05ff68f9ed8" # daniel.eszeki@dakai.io
    konstantin = "efca7ab3-ad09-41ab-90cc-354774532e2b" # konstantin.laveczky@dakai.io
    tamas      = "cebe5aaf-d5ea-4a0c-9fb2-12f9819432bc" # tamas.kiss@dakai.io
    mark       = "6c1c353b-f0b9-4c57-95a3-3eebba5cd440" # mark@dakai.io (Mark Ilsinszki)
    andras     = "2f555b93-2dca-44dd-bb80-e206b0fc7673" # andras.toth@dakai.io
  }
  sso_redirect = "https://${local.fqdn}/auth/callback"
}

resource "azuread_application" "sso" {
  display_name     = "conde"
  sign_in_audience = "AzureADMyOrg"
  notes            = "Human sign-in to conde (https://${local.fqdn}); assignment required, users listed in Terraform."

  web {
    # The CLI's `conde login` reuses the web flow and finishes on a loopback redirect (any port).
    redirect_uris = [local.sso_redirect, "http://localhost/auth/cli", "http://127.0.0.1/auth/cli"]
    logout_url    = "https://${local.fqdn}/"

    implicit_grant {
      id_token_issuance_enabled = false
    }
  }

  required_resource_access {
    resource_app_id = "00000003-0000-0000-c000-000000000000" # Microsoft Graph
    resource_access {
      id   = "37f7f235-527c-4136-accd-4a02d197296e" # openid
      type = "Scope"
    }
    resource_access {
      id   = "14dad69e-099b-42c9-810b-d002981feec1" # profile
      type = "Scope"
    }
    resource_access {
      id   = "64a6cdd6-aab1-4aaf-94b8-3cc8405e90d0" # email
      type = "Scope"
    }
  }
}

resource "azuread_service_principal" "sso" {
  client_id                    = azuread_application.sso.client_id
  app_role_assignment_required = true
}

resource "azuread_app_role_assignment" "sso_users" {
  for_each            = local.sso_users
  app_role_id         = "00000000-0000-0000-0000-000000000000" # default access
  principal_object_id = each.value
  resource_object_id  = azuread_service_principal.sso.object_id
}

# Placeholder the app's reference needs from the first apply; the real value is written by hand (README).
resource "azurerm_key_vault_secret" "entra_client_secret" {
  name             = "entra-client-secret"
  key_vault_id     = azurerm_key_vault.this.id
  value_wo         = "unset"
  value_wo_version = 1
  tags             = local.tags

  lifecycle {
    ignore_changes = [content_type, tags]
  }

  depends_on = [time_sleep.rbac]
}

output "sso" {
  value = {
    tenant_id    = data.azurerm_client_config.current.tenant_id
    client_id    = azuread_application.sso.client_id
    redirect_uri = local.sso_redirect
    issuer       = "https://login.microsoftonline.com/${data.azurerm_client_config.current.tenant_id}/v2.0"
    users        = keys(local.sso_users)
  }
}
