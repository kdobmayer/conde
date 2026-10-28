# ── GKE Autopilot: private nodes, control plane only through the DNS endpoint (IAM), Gateway API, Secret Manager CSI
resource "google_container_cluster" "conde" {
  name                = "conde"
  location            = local.region
  enable_autopilot    = true
  deletion_protection = true

  network    = local.foundation.network_self_link
  subnetwork = local.foundation.subnet_self_link

  ip_allocation_policy {
    cluster_secondary_range_name  = local.foundation.pods_range_name
    services_secondary_range_name = local.foundation.services_range_name
  }

  release_channel {
    channel = "REGULAR"
  }

  private_cluster_config {
    enable_private_nodes = true
  }

  control_plane_endpoints_config {
    dns_endpoint_config {
      allow_external_traffic = true
    }
    ip_endpoints_config {
      enabled = false
    }
  }

  # Hostname egress policies (FQDNNetworkPolicy); Autopilot enables it only on an existing cluster and restarts the nodes.
  enable_fqdn_network_policy = true

  gateway_api_config {
    channel = "CHANNEL_STANDARD"
  }

  secret_manager_config {
    enabled = true
    rotation_config {
      enabled = true
    }
  }

  security_posture_config {
    mode = "BASIC"
  }

  # System metrics only (free, and Autopilot keeps them on); the metric packages bill per sample ingested.
  monitoring_config {
    enable_components = ["SYSTEM_COMPONENTS"]
    managed_prometheus {
      enabled = true
    }
  }

  # Daily 01:00-05:00 UTC; the dates only anchor the recurrence.
  maintenance_policy {
    recurring_window {
      start_time = "2026-10-29T01:00:00Z"
      end_time   = "2026-10-29T05:00:00Z"
      recurrence = "FREQ=DAILY"
    }
  }
}

# The Autopilot nodes run as the project's default compute service account, which the organization policy
# leaves without roles; this is Google's documented minimum for node logging, metrics and image pulls.
data "google_project" "this" {
  project_id = local.project
}

resource "google_project_iam_member" "node_default_sa" {
  project = local.project
  role    = "roles/container.defaultNodeServiceAccount"
  member  = "serviceAccount:${data.google_project.this.number}-compute@developer.gserviceaccount.com"
}
