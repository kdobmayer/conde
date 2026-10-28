output "url" {
  value = "https://${var.hostname}"
}

output "namespace" {
  value = local.namespace
}

output "database" {
  value = google_sql_database.this.name
}

output "database_user" {
  value = google_sql_user.this.name
}

output "service_account" {
  value = google_service_account.this.email
}

output "oidc_issuer" {
  value = local.oidc_issuer
}

output "oidc_redirect_uri" {
  value = local.oidc_redirect_uri
}

output "secrets" {
  description = "Secret Manager secret ids by the file name they are mounted as."
  value       = local.app_secrets
}
