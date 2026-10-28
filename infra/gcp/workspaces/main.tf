# The conde workspaces on GCP (docs/infra-plan-gcp.md), the GCP side of each from modules/workspace; Argo CD runs the
# servers (deploy/tenants). Applied locally as terraform@conde-prod through impersonation, after the cluster root.
provider "google" {
  project                     = local.project
  region                      = local.region
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}

data "terraform_remote_state" "foundation" {
  backend = "gcs"
  config = {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "foundation"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}

data "terraform_remote_state" "platform" {
  backend = "gcs"
  config = {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "platform"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}

data "terraform_remote_state" "cluster" {
  backend = "gcs"
  config = {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "cluster"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}

locals {
  foundation = data.terraform_remote_state.foundation.outputs
  platform   = data.terraform_remote_state.platform.outputs
  cluster    = data.terraform_remote_state.cluster.outputs
  project    = "conde-prod"
  region     = "europe-west3"

  workspaces = {
    # Dakai signs in directly against the Dakai Entra tenant (the existing app registration), not through Keycloak.
    dakai = {
      hostname    = "dakai.conde.cloud"
      oidc_issuer = "https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0"
    }
    # Its server invites users through the realm's conde-admin client (infra/gcp/realms/demo.tf).
    demo       = { hostname = "demo.conde.cloud", extra_secrets = ["keycloak-admin-demo"] }
    clearscale = { hostname = "clearscale.conde.cloud" }
  }
}

module "workspace" {
  source   = "../modules/workspace"
  for_each = local.workspaces

  name          = each.key
  hostname      = each.value.hostname
  oidc_issuer   = lookup(each.value, "oidc_issuer", null)
  extra_secrets = lookup(each.value, "extra_secrets", [])

  platform = {
    project             = local.project
    project_number      = local.foundation.project_number
    region              = local.region
    sql_connection_name = local.platform.sql_connection_name
    keycloak_url        = local.cluster.keycloak.url
    pg_admin_secret     = "pg-admin-password"
  }
}
