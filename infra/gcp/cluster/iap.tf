# ── Identity-Aware Proxy in front of the admin backends: the Keycloak console (admin-auth.conde.cloud) and Argo CD ──────
# A custom OAuth client (External audience, so a Gmail account can pass; Google's managed client admits only users of
# the organization). Its secret is in Secret Manager as iap-admin-client-secret; GCPBackendPolicy reads it only from a
# Kubernetes Secret in the backend's namespace (README.md: Kubernetes Secrets that hold values).
locals {
  # Created by the owner (docs/infra-gcp-s4-iap.md), like the Google client secrets of the realms.
  iap_secret_manager_secret = "iap-admin-client-secret"
  # GCPBackendPolicy reads the client secret from the data key `key`.
  iap_kubernetes_secret = "iap-admin"
  iap_member            = "user:kdobmayer.devops@gmail.com"
  iap_namespaces = {
    keycloak = kubernetes_namespace_v1.keycloak.metadata[0].name
    argocd   = kubernetes_namespace_v1.argocd.metadata[0].name
  }

  iap_policy = {
    enabled            = true
    clientID           = var.iap_client_id
    oauth2ClientSecret = { name = local.iap_kubernetes_secret }
  }
}

# One explicit version, read during plan and apply only; the value is in neither the plan nor the state.
ephemeral "google_secret_manager_secret_version" "iap_client_secret" {
  project = local.project
  secret  = local.iap_secret_manager_secret
  version = tostring(var.iap_client_secret_version)
}

# Write-only data: the provider writes it only when the revision changes and never reads it back into state, so the
# revision is the version number read above. The provider logs the Secret object, value included, at INFO on create and
# update: apply this root with TF_LOG unset.
resource "kubernetes_secret_v1" "iap" {
  for_each = local.iap_namespaces

  metadata {
    name      = local.iap_kubernetes_secret
    namespace = each.value
  }

  data_wo          = { key = ephemeral.google_secret_manager_secret_version.iap_client_secret.secret_data }
  data_wo_revision = var.iap_client_secret_version
}

# ── Who passes IAP: the owner, on the two admin backends only. The Gateway controller names the backend services and
# lists them on the Gateway; a backend created in this apply appears there afterwards, so the first apply of a new
# admin backend is followed by a second one that adds its grant (the check below warns until then). ───────────────
data "kubernetes_resource" "gateway" {
  api_version = "gateway.networking.k8s.io/v1"
  kind        = "Gateway"
  metadata {
    name      = "conde"
    namespace = "gateway"
  }
}

locals {
  gateway_backend_services = [
    for s in split(",", try(data.kubernetes_resource.gateway.object.metadata.annotations["networking.gke.io/backend-services"], "")) :
    basename(trimspace(s)) if trimspace(s) != ""
  ]
  iap_backend_patterns = {
    keycloak-admin = "^gkegw1-[a-z0-9]+-keycloak-keycloak-admin-8080-[a-z0-9]+$"
    argocd         = "^gkegw1-[a-z0-9]+-argocd-argocd-server-80-[a-z0-9]+$"
  }
  iap_backend_matches = {
    for k, pattern in local.iap_backend_patterns : k => [for n in local.gateway_backend_services : n if can(regex(pattern, n))]
  }
  iap_backend_services = { for k, names in local.iap_backend_matches : k => one(names) if length(names) == 1 }
}

check "iap_backend_services" {
  assert {
    condition     = length(local.iap_backend_services) == length(local.iap_backend_patterns)
    error_message = "IAP grant missing for: ${join(", ", [for k, names in local.iap_backend_matches : k if length(names) != 1])} (backend service not yet created, or not unique); apply again once the Gateway lists it."
  }
}

resource "google_iap_web_backend_service_iam_member" "admin" {
  for_each = local.iap_backend_services

  project             = local.project
  web_backend_service = each.value
  role                = "roles/iap.httpsResourceAccessor"
  member              = local.iap_member
}
