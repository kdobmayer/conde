# Kubernetes layer of conde on GCP shared by every workspace (docs/infra-plan-gcp.md): the Gateway, Keycloak and Argo CD.
# Applied locally as terraform@conde-prod through impersonation; the API server is reached on the cluster's DNS
# endpoint with that identity's access token (a public CA certificate, so no cluster CA is configured).
provider "google" {
  project                     = local.project
  region                      = local.region
  impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
}

data "google_client_config" "this" {}

provider "kubernetes" {
  host  = "https://${local.platform.cluster_dns_endpoint}"
  token = data.google_client_config.this.access_token
}

provider "helm" {
  kubernetes = {
    host  = "https://${local.platform.cluster_dns_endpoint}"
    token = data.google_client_config.this.access_token
  }
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

locals {
  foundation    = data.terraform_remote_state.foundation.outputs
  platform      = data.terraform_remote_state.platform.outputs
  project       = "conde-prod"
  region        = "europe-west3"
  wi_pool       = "${local.project}.svc.id.goog"
  gateway_label = { "conde.cloud/gateway" = "shared" }
  # Google Front End and health-check source ranges of the global external Application Load Balancer.
  gfe_ranges        = ["35.191.0.0/16", "130.211.0.0/22"]
  private_ranges    = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
  cloud_sql_proxy   = "gcr.io/cloud-sql-connectors/cloud-sql-proxy:2.26.3@sha256:f333a9c59890c3c67b8505166c5dbdcc0b29fca9173a2e09ca0446ab6997a736"
  keycloak_image    = "${local.foundation.registry_url}/keycloak:26.8.1-conde1@sha256:89cdb465103c5a9107419a0202d77a30e4ce70d236f1d8c809fc680aba33a8f6"
  keycloak_hostname = "auth.conde.cloud"
  network_policy_egress = [
    # NodeLocal DNSCache, the pods' resolver; it runs on the host network, so no pod selector matches it.
    { cidr = "169.254.20.10/32", except = [], ports = [{ protocol = "UDP", port = "53" }, { protocol = "TCP", port = "53" }] },
    { cidr = "${local.foundation.psc_address}/32", except = [], ports = [{ protocol = "TCP", port = "3307" }] },
    # The GKE metadata server (Workload Identity tokens): its address, and its port on the node under Dataplane V2.
    { cidr = "169.254.169.254/32", except = [], ports = [{ protocol = "TCP", port = "80" }] },
    { cidr = "169.254.169.252/32", except = [], ports = [{ protocol = "TCP", port = "988" }] },
    { cidr = "0.0.0.0/0", except = local.private_ranges, ports = [{ protocol = "TCP", port = "443" }] },
  ]
}

# ── Nodes: Autopilot runs them as the default compute service account, which has no roles in this project ─────
resource "google_artifact_registry_repository_iam_member" "nodes_pull" {
  project    = local.project
  location   = local.region
  repository = "conde"
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${local.foundation.project_number}-compute@developer.gserviceaccount.com"
}
