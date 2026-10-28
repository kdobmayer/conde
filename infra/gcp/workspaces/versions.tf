terraform {
  required_version = ">= 1.16.0"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "8.9.0"
    }
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = "3.4.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "3.9.1"
    }
  }

  backend "gcs" {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "workspaces"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}
