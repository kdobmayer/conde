# ── Identity-Aware Proxy in front of the admin backends: the Keycloak console (admin-auth.conde.cloud) and Argo CD ──────
# A custom OAuth client (External audience, so a Gmail account can pass; Google's managed client admits only users of
# the organization). Its secret is in Secret Manager as iap-admin-client-secret; GCPBackendPolicy reads it from a
# Kubernetes Secret in the backend's namespace, so a writer Job copies it there (README.md: Kubernetes Secrets that
# hold values).
locals {
  iap_secret_import = "iap-secret-import"
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

  # The writer of the repo-key hook (argocd.tf) for the IAP secret, one per namespace, carried as pre-install and
  # pre-upgrade hooks of the Argo CD release (the estate's only Helm release): the Secret exists before a policy
  # names it, and the TTL controller deletes the pod, with its token, ten minutes after it finishes.
  iap_secret_import_jobs = [for ns in values(local.iap_namespaces) : {
    apiVersion = "batch/v1"
    kind       = "Job"
    metadata = {
      name      = "${local.iap_secret_import}-hook"
      namespace = ns
      annotations = {
        "helm.sh/hook"                          = "pre-install,pre-upgrade"
        "helm.sh/hook-delete-policy"            = "before-hook-creation"
        "conde.cloud/iap-client-secret-version" = tostring(var.iap_client_secret_version)
      }
    }
    spec = {
      backoffLimit            = 4
      ttlSecondsAfterFinished = 600
      template = {
        metadata = { labels = { "app.kubernetes.io/name" = local.iap_secret_import } }
        spec = {
          serviceAccountName = local.iap_secret_import
          restartPolicy      = "Never"
          securityContext = {
            runAsNonRoot   = true
            runAsUser      = 65532
            seccompProfile = { type = "RuntimeDefault" }
          }
          containers = [{
            name  = "kubectl"
            image = local.kubectl_image
            args  = ["apply", "--server-side", "--field-manager=${local.iap_secret_import}", "-k", "/work"]
            env   = [{ name = "HOME", value = "/tmp" }]
            resources = {
              requests = { cpu = "50m", memory = "64Mi", "ephemeral-storage" = "100Mi" }
              limits   = { "ephemeral-storage" = "100Mi" }
            }
            securityContext = {
              allowPrivilegeEscalation = false
              readOnlyRootFilesystem   = true
              capabilities             = { drop = ["ALL"] }
            }
            volumeMounts = [
              { name = "work", mountPath = "/work" },
              { name = "kustomization", mountPath = "/work/kustomization.yaml", subPath = "kustomization.yaml", readOnly = true },
              { name = "secret", mountPath = "/work/secret", readOnly = true },
              { name = "tmp", mountPath = "/tmp" },
            ]
          }]
          volumes = [
            { name = "work", emptyDir = {} },
            { name = "tmp", emptyDir = {} },
            { name = "kustomization", configMap = { name = local.iap_secret_import } },
            {
              name = "secret"
              csi = {
                driver           = "secrets-store-gke.csi.k8s.io"
                readOnly         = true
                volumeAttributes = { secretProviderClass = local.iap_secret_import }
              }
            },
          ]
        }
      }
    }
  }]
}

resource "google_secret_manager_secret_iam_member" "iap_secret_import" {
  for_each = local.iap_namespaces

  project   = local.project
  secret_id = "projects/${local.project}/secrets/${local.iap_secret_manager_secret}"
  role      = "roles/secretmanager.secretAccessor"
  member    = "principal://iam.googleapis.com/projects/${local.foundation.project_number}/locations/global/workloadIdentityPools/${local.wi_pool}/subject/ns/${each.value}/sa/${local.iap_secret_import}"
}

resource "kubernetes_service_account_v1" "iap_secret_import" {
  for_each = local.iap_namespaces

  metadata {
    name      = local.iap_secret_import
    namespace = each.value
  }
}

# Create cannot be limited by name in RBAC; reading and changing are limited to the one Secret.
resource "kubernetes_role_v1" "iap_secret_import" {
  for_each = local.iap_namespaces

  metadata {
    name      = local.iap_secret_import
    namespace = each.value
  }

  rule {
    api_groups = [""]
    resources  = ["secrets"]
    verbs      = ["create"]
  }

  rule {
    api_groups     = [""]
    resources      = ["secrets"]
    resource_names = [local.iap_kubernetes_secret]
    verbs          = ["get", "patch"]
  }
}

resource "kubernetes_role_binding_v1" "iap_secret_import" {
  for_each = local.iap_namespaces

  metadata {
    name      = local.iap_secret_import
    namespace = each.value
  }

  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "Role"
    name      = kubernetes_role_v1.iap_secret_import[each.key].metadata[0].name
  }

  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.iap_secret_import[each.key].metadata[0].name
    namespace = each.value
  }
}

resource "kubernetes_manifest" "iap_secret_import" {
  for_each = local.iap_namespaces

  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = local.iap_secret_import
      namespace = each.value
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([{
          resourceName = "projects/${local.project}/secrets/${local.iap_secret_manager_secret}/versions/latest"
          path         = "client-secret"
        }])
      }
    }
  }
}

resource "kubernetes_config_map_v1" "iap_secret_import" {
  for_each = local.iap_namespaces

  metadata {
    name      = local.iap_secret_import
    namespace = each.value
  }

  data = {
    "kustomization.yaml" = yamlencode({
      apiVersion       = "kustomize.config.k8s.io/v1beta1"
      kind             = "Kustomization"
      namespace        = each.value
      generatorOptions = { disableNameSuffixHash = true }
      secretGenerator = [{
        name  = local.iap_kubernetes_secret
        files = ["key=secret/client-secret"]
      }]
    })
  }
}

# Keycloak's own policy keeps private ranges closed on 443; the writer alone also reaches the API server.
resource "kubernetes_network_policy_v1" "keycloak_iap_secret_import" {
  metadata {
    name      = local.iap_secret_import
    namespace = local.iap_namespaces.keycloak
  }

  spec {
    pod_selector {
      match_labels = { "app.kubernetes.io/name" = local.iap_secret_import }
    }
    policy_types = ["Egress"]

    egress {
      dynamic "to" {
        for_each = local.api_server_ips
        content {
          ip_block {
            cidr = "${to.value}/32"
          }
        }
      }
      ports {
        protocol = "TCP"
        port     = "443"
      }
    }
  }
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
