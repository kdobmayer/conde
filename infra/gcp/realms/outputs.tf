output "issuers" {
  value = { for name in keys(local.realms) : name => "${local.keycloak_url}/realms/${name}" }
}

output "clients" {
  value = merge(
    { for name in keys(local.tenants) : name => { client_id = keycloak_openid_client.conde[name].client_id, secret = "${name}-oidc-client-secret" } },
    { infra = { client_id = keycloak_openid_client.argocd.client_id, admin_group = keycloak_group.infra_admins.name } },
  )
}

# Secret Manager secret names; the values are only there.
output "generated_secrets" {
  value = sort(keys(google_secret_manager_secret.generated))
}
