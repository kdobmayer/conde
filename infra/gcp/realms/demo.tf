# ── demo: the admin API client conde's server invites users with, and the user profile that holds the invite ────
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

# Keycloak 26's default profile, plus conde_invite: with unmanaged attributes disabled, Keycloak drops an attribute the
# profile does not declare. Admins only, so the account console neither shows nor edits it. First and last name are
# optional, so an invited user goes from the password straight to the board.
resource "keycloak_realm_user_profile" "demo" {
  realm_id = keycloak_realm.this["demo"].id

  attribute {
    name         = "username"
    display_name = "$${username}"

    validator {
      name   = "length"
      config = { min = "3", max = "255" }
    }
    validator {
      name = "username-prohibited-characters"
    }
    validator {
      name = "up-username-not-idn-homograph"
    }

    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
  }

  attribute {
    name               = "email"
    display_name       = "$${email}"
    required_for_roles = ["user"]

    validator {
      name = "email"
    }
    validator {
      name   = "length"
      config = { max = "255" }
    }

    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
  }

  attribute {
    name         = "firstName"
    display_name = "$${firstName}"

    validator {
      name   = "length"
      config = { max = "255" }
    }
    validator {
      name = "person-name-prohibited-characters"
    }

    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
  }

  attribute {
    name         = "lastName"
    display_name = "$${lastName}"

    validator {
      name   = "length"
      config = { max = "255" }
    }
    validator {
      name = "person-name-prohibited-characters"
    }

    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
  }

  attribute {
    name         = "conde_invite"
    display_name = "conde invite"

    permissions {
      view = ["admin"]
      edit = ["admin"]
    }
  }

  group {
    name                = "user-metadata"
    display_header      = "User metadata"
    display_description = "Attributes, which refer to user metadata"
  }
}
