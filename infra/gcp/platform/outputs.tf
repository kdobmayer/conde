output "cluster_name" {
  value = google_container_cluster.conde.name
}

output "cluster_location" {
  value = google_container_cluster.conde.location
}

output "cluster_dns_endpoint" {
  value = google_container_cluster.conde.control_plane_endpoints_config[0].dns_endpoint_config[0].endpoint
}

output "gateway_ip_address" {
  value = google_compute_global_address.gateway.address
}

output "gateway_ip_name" {
  value = google_compute_global_address.gateway.name
}

output "certificate_map_name" {
  value = google_certificate_manager_certificate_map.conde.name
}

output "sql_connection_name" {
  value = google_sql_database_instance.pg.connection_name
}

output "sql_dns_name" {
  value = google_sql_database_instance.pg.dns_name
}

output "cloud_armor_policy_name" {
  value = google_compute_security_policy.web.name
}

output "dns_authorization_record" {
  description = "Add at Cloudflare (DNS only, not proxied) so the certificate can be issued."
  value = {
    name = google_certificate_manager_dns_authorization.conde.dns_resource_record[0].name
    type = google_certificate_manager_dns_authorization.conde.dns_resource_record[0].type
    data = google_certificate_manager_dns_authorization.conde.dns_resource_record[0].data
  }
}

output "dns_a_records" {
  description = "Add at Cloudflare (DNS only, not proxied): the apex and the wildcard to the Gateway address."
  value = {
    (local.domain)        = google_compute_global_address.gateway.address
    ("*.${local.domain}") = google_compute_global_address.gateway.address
  }
}
