output "gateway" {
  value = {
    name      = kubernetes_manifest.gateway.manifest.metadata.name
    namespace = kubernetes_namespace_v1.gateway.metadata[0].name
    address   = local.platform.gateway_ip_address
  }
}

output "keycloak" {
  value = {
    url                    = "https://${local.keycloak_hostname}"
    admin_console          = "https://${local.keycloak_admin_hostname}/admin/master/console/"
    image                  = local.keycloak_image
    bootstrap_admin_user   = "bootstrap-admin"
    bootstrap_admin_secret = google_secret_manager_secret.keycloak_bootstrap_admin.secret_id
    service_account        = google_service_account.keycloak.email
  }
}

# The workspaces root reads these instead of repeating them.
output "cloud_sql_proxy_image" {
  value = local.cloud_sql_proxy
}

output "network_policy_egress" {
  value = local.network_policy_egress
}
