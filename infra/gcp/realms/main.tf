# Keycloak configuration of conde on GCP (docs/infra-plan-gcp.md): a realm per workspace that signs in through
# Keycloak, the `infra` realm for platform tools, and the permanent admins of the master realm. Applied locally from
# the admin allowlist, since the provider calls /admin and /realms/master (README.md).
provider "google" {
  project                     = local.project
  region                      = local.region
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}

# Client id and credentials come from the environment at run time (README.md).
provider "keycloak" {
  url   = local.keycloak_url
  realm = "master"
}

locals {
  project      = "conde-prod"
  region       = "europe-west3"
  keycloak_url = "https://auth.conde.cloud"
  owner_email  = "kdobmayer.devops@gmail.com"

  # Workspaces that sign in through Keycloak; dakai signs in directly against its Entra tenant.
  tenants = {
    demo       = { display_name = "conde demo", hostname = "demo.conde.cloud" }
    clearscale = { display_name = "ClearScale", hostname = "clearscale.conde.cloud" }
  }

  # Tenant realms that sign in only through Google; the OAuth client secret is in <name>-google-oauth-client-secret.
  google_sign_in = {
    clearscale = { hosted_domain = "clearscale.com", client_id = "327673226052-m2lh9vekck7mhp473m4gk6h1qfi8fjpq.apps.googleusercontent.com" }
  }

  # passwords: the realm signs its users in with Keycloak passwords, so it offers "Forgot password"; infra does not.
  realms = merge(
    { for name, tenant in local.tenants : name => {
      display_name = tenant.display_name
      require_mfa  = var.require_mfa[name]
      passwords    = !contains(keys(local.google_sign_in), name)
    } },
    { infra = { display_name = "conde infra", require_mfa = true, passwords = false } },
  )

  # Outbound mail of every realm, master included, through Resend (SMTPS); the password is the Resend API key.
  smtp = {
    host              = "smtp.resend.com"
    port              = "465"
    username          = "resend"
    from              = "no-reply@conde.cloud"
    from_display_name = "conde"
    reply_to          = local.owner_email
  }
}

# The provider has no write-only SMTP password, so the API key is in this state, like the Google client secret.
data "google_secret_manager_secret_version" "resend" {
  secret = "resend-api-key"
}

# Secrets this root writes; every value is write-only, so none is in this state.
resource "google_secret_manager_secret" "generated" {
  for_each = toset(["infra-krisztian-temp-password", "keycloak-admin-demo", "keycloak-admin-temp-password", "keycloak-terraform-client-secret"])

  secret_id = each.value

  replication {
    user_managed {
      replicas {
        location = local.region
      }
    }
  }
}
