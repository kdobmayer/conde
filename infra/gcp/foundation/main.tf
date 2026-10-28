# Foundation of conde on GCP (docs/infra-plan-gcp.md): APIs, network, NAT, Artifact Registry, the dist bucket,
# GitHub federation. Applied locally as terraform@conde-prod through impersonation; state in the gcs backend.
provider "google" {
  project                     = local.project
  region                      = local.region
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}

locals {
  project = "conde-prod"
  region  = "europe-west3"
  services = [
    "artifactregistry.googleapis.com",
    "certificatemanager.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "compute.googleapis.com",
    "container.googleapis.com",
    "containersecurity.googleapis.com",
    "dns.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "logging.googleapis.com",
    "monitoring.googleapis.com",
    "networkservices.googleapis.com",
    "secretmanager.googleapis.com",
    "serviceusage.googleapis.com",
    "sqladmin.googleapis.com",
    "storage.googleapis.com",
    "sts.googleapis.com",
  ]
  github_enabled = var.github_repository_owner_id != "" && var.github_repository_id != ""
}

data "google_project" "this" {}

resource "google_project_service" "this" {
  for_each           = toset(local.services)
  service            = each.value
  disable_on_destroy = false
}

# ── Network: one custom VPC, one subnet for GKE (nodes, pods, services) and the Cloud SQL PSC endpoint ───────────
resource "google_compute_network" "conde" {
  name                    = "conde"
  auto_create_subnetworks = false
  routing_mode            = "REGIONAL"

  depends_on = [google_project_service.this]
}

resource "google_compute_subnetwork" "gke" {
  name                     = "gke-${local.region}"
  network                  = google_compute_network.conde.id
  region                   = local.region
  ip_cidr_range            = "10.10.0.0/20"
  private_ip_google_access = true

  secondary_ip_range {
    range_name    = "pods"
    ip_cidr_range = "10.20.0.0/16"
  }

  secondary_ip_range {
    range_name    = "services"
    ip_cidr_range = "10.30.0.0/20"
  }
}

resource "google_compute_address" "psc" {
  name         = "psc"
  region       = local.region
  subnetwork   = google_compute_subnetwork.gke.id
  address_type = "INTERNAL"
  description  = "Private Service Connect endpoint of the Cloud SQL instance conde-pg"
}

resource "google_compute_router" "conde" {
  name    = "conde"
  region  = local.region
  network = google_compute_network.conde.id
}

resource "google_compute_router_nat" "conde" {
  name                               = "conde"
  router                             = google_compute_router.conde.name
  region                             = local.region
  nat_ip_allocate_option             = "AUTO_ONLY"
  source_subnetwork_ip_ranges_to_nat = "ALL_SUBNETWORKS_ALL_IP_RANGES"

  log_config {
    enable = true
    filter = "ERRORS_ONLY"
  }
}

# ── Artifact Registry: the server image ─────────────────────────────────────────────────────────────────────────
resource "google_artifact_registry_repository" "conde" {
  repository_id          = "conde"
  location               = local.region
  format                 = "DOCKER"
  cleanup_policy_dry_run = false

  cleanup_policies {
    id     = "delete-tagged"
    action = "DELETE"
    condition {
      tag_state = "TAGGED"
    }
  }

  # A KEEP policy wins over DELETE: the 20 newest versions survive.
  cleanup_policies {
    id     = "keep-20-newest"
    action = "KEEP"
    most_recent_versions {
      keep_count = 20
    }
  }

  depends_on = [google_project_service.this]
}

# ── Release binaries: public read, install.sh downloads from here ──────────────────────────────────────────────
resource "google_storage_bucket" "dist" {
  name                        = "conde-cloud-dist"
  location                    = upper(local.region)
  storage_class               = "STANDARD"
  uniform_bucket_level_access = true
  public_access_prevention    = "inherited"

  versioning {
    enabled = false
  }
}

resource "google_storage_bucket_iam_member" "dist_public" {
  bucket = google_storage_bucket.dist.name
  role   = "roles/storage.objectViewer"
  member = "allUsers"
}

# ── GitHub Actions: Workload Identity Federation to the deploy service account (no stored key) ─────────────────
resource "google_iam_workload_identity_pool" "github" {
  workload_identity_pool_id = "github"
  display_name              = "GitHub Actions"

  depends_on = [google_project_service.this]
}

resource "google_iam_workload_identity_pool_provider" "github" {
  count = local.github_enabled ? 1 : 0

  workload_identity_pool_id          = google_iam_workload_identity_pool.github.workload_identity_pool_id
  workload_identity_pool_provider_id = "github-actions"
  display_name                       = "GitHub Actions OIDC"
  attribute_mapping = {
    "google.subject"                = "assertion.sub"
    "attribute.repository"          = "assertion.repository"
    "attribute.repository_id"       = "assertion.repository_id"
    "attribute.repository_owner_id" = "assertion.repository_owner_id"
    "attribute.ref"                 = "assertion.ref"
    "attribute.environment"         = "assertion.environment"
  }
  # Numeric ids, not names: a renamed or re-created repository cannot take over the trust.
  attribute_condition = "assertion.repository_owner_id == '${var.github_repository_owner_id}' && assertion.repository_id == '${var.github_repository_id}'"

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }
}

resource "google_service_account" "deploy" {
  account_id   = "deploy"
  display_name = "GitHub Actions deploy (image push, release binaries, GKE rollout)"

  depends_on = [google_project_service.this]
}

resource "google_service_account_iam_member" "deploy_github" {
  count = local.github_enabled ? 1 : 0

  service_account_id = google_service_account.deploy.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.github.name}/attribute.repository_id/${var.github_repository_id}"
}

resource "google_artifact_registry_repository_iam_member" "deploy_writer" {
  location   = google_artifact_registry_repository.conde.location
  repository = google_artifact_registry_repository.conde.name
  role       = "roles/artifactregistry.writer"
  member     = google_service_account.deploy.member
}

resource "google_storage_bucket_iam_member" "deploy_dist" {
  bucket = google_storage_bucket.dist.name
  role   = "roles/storage.objectAdmin"
  member = google_service_account.deploy.member
}

resource "google_project_iam_member" "deploy_gke" {
  project = local.project
  role    = "roles/container.developer"
  member  = google_service_account.deploy.member
}
