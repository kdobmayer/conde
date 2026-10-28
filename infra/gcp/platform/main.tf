# Platform of conde on GCP (docs/infra-plan-gcp.md): Cloud SQL behind Private Service Connect, GKE Autopilot, the
# public entry (static IP, certificate, Cloud Armor) and the Keycloak database. Workspaces live in their own root.
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

locals {
  foundation = data.terraform_remote_state.foundation.outputs
  project    = "conde-prod"
  region     = "europe-west3"
  domain     = "conde.cloud"
}
