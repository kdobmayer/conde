output "project_id" {
  value = local.project
}

output "project_number" {
  value = data.google_project.this.number
}

output "region" {
  value = local.region
}

output "network_self_link" {
  value = google_compute_network.conde.self_link
}

output "network_id" {
  value = google_compute_network.conde.id
}

output "subnet_self_link" {
  value = google_compute_subnetwork.gke.self_link
}

output "pods_range_name" {
  value = google_compute_subnetwork.gke.secondary_ip_range[0].range_name
}

output "services_range_name" {
  value = google_compute_subnetwork.gke.secondary_ip_range[1].range_name
}

output "psc_address" {
  value = google_compute_address.psc.address
}

output "psc_address_self_link" {
  value = google_compute_address.psc.self_link
}

output "registry_url" {
  value = "${google_artifact_registry_repository.conde.location}-docker.pkg.dev/${local.project}/${google_artifact_registry_repository.conde.repository_id}"
}

output "dist_bucket_url" {
  value = "https://storage.googleapis.com/${google_storage_bucket.dist.name}"
}

output "deploy_service_account" {
  value = google_service_account.deploy.email
}

output "wif_provider_name" {
  description = "Full provider name for google-github-actions/auth; null until both GitHub ids are set."
  value       = local.github_enabled ? google_iam_workload_identity_pool_provider.github[0].name : null
}
