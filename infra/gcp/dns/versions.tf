terraform {
  required_version = ">= 1.16.0"

  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "5.28.1"
    }
    google = {
      source  = "hashicorp/google"
      version = "8.9.0"
    }
  }

  backend "gcs" {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "dns"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}

# The token comes from CLOUDFLARE_API_TOKEN at run time (pass show conde/cloudflare-api-token); never a variable.
provider "cloudflare" {}

provider "google" {
  project                     = "conde-prod"
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}
