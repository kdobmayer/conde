# Keycloak configuration of conde on GCP (docs/infra-plan-gcp.md): a realm per workspace that signs in through
# Keycloak, the `infra` realm for platform tools, and the permanent admins of the master realm. Applied through
# run.sh, which reaches the Keycloak Service over a port-forward (README.md).
provider "google" {
  project                     = local.project
  region                      = local.region
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}

# Client id and credentials come from the environment at run time (README.md).
provider "keycloak" {
  url   = var.provider_url
  realm = "master"
}

locals {
  project      = "conde-prod"
  region       = "europe-west3"
  keycloak_url = "https://auth.conde.cloud"
  # The master realm's own URL (its sign-in and the admin console, behind IAP); the other realms stay on auth.conde.cloud.
  master_url  = "https://admin-auth.conde.cloud"
  owner_email = "krisztian@conde.cloud"

  # Workspaces that sign in through Keycloak; dakai signs in directly against its Entra tenant.
  tenants = {
    demo       = { display_name = "conde demo", hostname = "demo.conde.cloud" }
    clearscale = { display_name = "ClearScale", hostname = "clearscale.conde.cloud" }
    vvpusa     = { display_name = "VVP USA", hostname = "vvpusa.conde.cloud" }
  }

  # Tenant realms that sign in only through Google; the OAuth client secret is in <name>-google-oauth-client-secret.
  google_sign_in = {
    clearscale = { hosted_domain = "clearscale.com", client_id = "327673226052-m2lh9vekck7mhp473m4gk6h1qfi8fjpq.apps.googleusercontent.com" }
  }

  # Tenant realms that sign in only through a company's SAML identity provider (saml.tf): the provider's metadata file
  # (public: its endpoints and signing certificate), and the alias Keycloak's broker endpoint carries
  # (/realms/<name>/broker/<alias>/endpoint). vvpusa: VVP USA's AWS IAM Identity Center application.
  saml_sign_in = {
    vvpusa = { alias = "aws", display_name = "VVP USA", metadata = file("${path.module}/vvpusa-aws-metadata.xml") }
  }

  # passwords: the realm signs its users in with Keycloak passwords, so it offers "Forgot password"; infra does not.
  realms = merge(
    { for name, tenant in local.tenants : name => {
      display_name = tenant.display_name
      require_mfa  = var.require_mfa[name]
      passwords    = !contains(concat(keys(local.google_sign_in), keys(local.saml_sign_in)), name)
    } },
    { infra = { display_name = "conde infra", require_mfa = true, passwords = false } },
  )
  # Tenant realms whose users get Keycloak's invitation email (the conde email theme).
  password_tenants = { for name, tenant in local.tenants : name => tenant if local.realms[name].passwords }
  # Tenant realms with demo's user profile (realms.tf); a Google realm keeps Keycloak's default.
  profile_realms = setsubtract(keys(local.tenants), keys(local.google_sign_in))

  # Outbound mail of every realm, master included, through Resend (SMTPS). The password is a reference to Keycloak's
  # file vault, which holds the Resend API key (`resend-api-key`, mounted by the cluster root), so it is not in this state.
  smtp = {
    host              = "smtp.resend.com"
    port              = "465"
    username          = "resend"
    password          = "$${vault.smtp-password}"
    from              = "no-reply@conde.cloud"
    from_display_name = "conde"
    reply_to          = local.owner_email
  }
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
