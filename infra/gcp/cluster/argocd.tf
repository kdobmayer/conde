# ── Argo CD: delivers the workspaces from charts/conde-workspace and deploy/ of github.com/kdobmayer/conde ─────────
locals {
  argocd_namespace = "argocd"
  argocd_hostname  = "argocd.conde.cloud"
  argocd_repo_url  = "git@github.com:kdobmayer/conde.git"
  # GitHub's own ranges from the "git" list of https://api.github.com/meta; github.com resolves into them from Europe.
  github_ranges = ["192.30.252.0/22", "185.199.108.0/22", "140.82.112.0/20", "143.55.64.0/20"]
  # The "ssh_keys" of https://api.github.com/meta, matching the fingerprints GitHub documents.
  github_host_keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=",
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=",
  ]
  argocd_repo_import = "argocd-repo-import"
  # Granted as its own Workload Identity principal, scoped to the one secret it mounts.
  argocd_repo_import_principal = "principal://iam.googleapis.com/projects/${local.foundation.project_number}/locations/global/workloadIdentityPools/${local.wi_pool}/subject/ns/${local.argocd_namespace}/sa/${local.argocd_repo_import}"
  kubectl_image                = "registry.k8s.io/kubectl:v1.36.5@sha256:0667e3f3e6ef435d7cd4c16684244703ebd67314c17511c2a9f87ea44f6cdf6b"
  # Pods reach the API server at its in-VPC address; the cluster object reports none with IP endpoints disabled.
  api_server_ips = [for e in data.kubernetes_resource.api_server.object.endpoints : one(e.addresses)]

  workspace_domain = "conde.cloud"
  postgres_image   = "docker.io/library/postgres:18.6-alpine@sha256:10a5c69a83035ab5239f5620d708520e8b28f5aa1f2b20143e6024532edadfb5"

  # Writes only the repository Secret, by server-side apply (no last-applied annotation copying the key); kubectl prints
  # the object name only. A Helm hook of the Argo CD release, so it runs on every install and upgrade, and the TTL
  # controller deletes it and its pod, with the pod's token, ten minutes after it finishes; Terraform tracks no Job.
  argocd_repo_import_job = {
    apiVersion = "batch/v1"
    kind       = "Job"
    metadata = {
      name      = "${local.argocd_repo_import}-hook"
      namespace = local.argocd_namespace
      annotations = {
        "helm.sh/hook"                 = "post-install,post-upgrade"
        "helm.sh/hook-delete-policy"   = "before-hook-creation"
        "conde.cloud/repo-key-version" = tostring(var.argocd_repo_key_version)
      }
    }
    spec = {
      backoffLimit            = 4
      ttlSecondsAfterFinished = 600
      template = {
        metadata = { labels = { "app.kubernetes.io/name" = local.argocd_repo_import } }
        spec = {
          serviceAccountName = local.argocd_repo_import
          restartPolicy      = "Never"
          securityContext = {
            runAsNonRoot   = true
            runAsUser      = 65532
            seccompProfile = { type = "RuntimeDefault" }
          }
          containers = [{
            name  = "kubectl"
            image = local.kubectl_image
            args  = ["apply", "--server-side", "--field-manager=${local.argocd_repo_import}", "-k", "/work"]
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
              { name = "key", mountPath = "/work/key", readOnly = true },
              { name = "tmp", mountPath = "/tmp" },
            ]
          }]
          volumes = [
            { name = "work", emptyDir = {} },
            { name = "tmp", emptyDir = {} },
            { name = "kustomization", configMap = { name = local.argocd_repo_import } },
            {
              name = "key"
              csi = {
                driver           = "secrets-store-gke.csi.k8s.io"
                readOnly         = true
                volumeAttributes = { secretProviderClass = local.argocd_repo_import }
              }
            },
          ]
        }
      }
    }
  }

  # Platform values of every workspace Application; the ApplicationSet derives the per-workspace values.
  workspace_platform_values = {
    namespace   = { create = true }
    distBaseUrl = local.foundation.dist_bucket_url
    route = {
      gateway = {
        name        = kubernetes_manifest.gateway.manifest.metadata.name
        namespace   = kubernetes_namespace_v1.gateway.metadata[0].name
        sectionName = "https"
      }
    }
    networkPolicy = { egress = local.network_policy_egress }
    gcp = {
      enabled            = true
      project            = local.project
      sqlConnectionName  = local.platform.sql_connection_name
      cloudSqlProxyImage = local.cloud_sql_proxy
      securityPolicy     = local.platform.cloud_armor_policy_name
    }
  }

  argocd_values = {
    global = {
      domain = local.argocd_hostname
      # Replaced by the namespace-wide policy below, which also limits egress.
      networkPolicy = { create = false }
    }
    configs = {
      # TLS ends at the load balancer; the Gateway reaches the server over plain HTTP inside the VPC.
      params = { "server.insecure" = true }
      cm = {
        "admin.enabled" = tostring(var.argocd_local_admin)
        "oidc.config" = yamlencode({
          name                     = "Keycloak"
          issuer                   = var.argocd_oidc_issuer
          clientID                 = var.argocd_oidc_client_id
          enablePKCEAuthentication = true
          requestedScopes          = ["openid", "profile", "email", "groups"]
        })
      }
      rbac = {
        "policy.default" = ""
        "policy.csv"     = "g, ${var.argocd_admin_group}, role:admin\n"
        scopes           = "[groups]"
      }
      ssh = { knownHosts = join("", [for k in local.github_host_keys : "github.com ${k}\n"]) }
    }
    dex           = { enabled = false }
    notifications = { enabled = false }
    controller = {
      resources = { requests = { cpu = "250m", memory = "512Mi" } }
    }
    repoServer = {
      resources = { requests = { cpu = "250m", memory = "512Mi" } }
    }
    applicationSet = {
      resources = { requests = { cpu = "100m", memory = "128Mi" } }
    }
    redis = {
      resources = { requests = { cpu = "100m", memory = "128Mi" } }
    }
    server = {
      resources = { requests = { cpu = "100m", memory = "256Mi" } }
      httproute = {
        enabled = true
        parentRefs = [{
          name        = kubernetes_manifest.gateway.manifest.metadata.name
          namespace   = kubernetes_namespace_v1.gateway.metadata[0].name
          sectionName = "https"
        }]
        hostnames = [local.argocd_hostname]
      }
    }
    extraObjects = [
      {
        apiVersion = "networking.gke.io/v1"
        kind       = "HealthCheckPolicy"
        metadata   = { name = "argocd-server", namespace = local.argocd_namespace }
        spec = {
          default = {
            checkIntervalSec   = 10
            timeoutSec         = 5
            healthyThreshold   = 1
            unhealthyThreshold = 3
            config = {
              type            = "HTTP"
              httpHealthCheck = { port = 8080, requestPath = "/healthz" }
            }
          }
          targetRef = { group = "", kind = "Service", name = "argocd-server" }
        }
      },
      # The UI's watch streams stay open; Cloud Armor holds the whole host to admin_cidrs (platform rule 1003).
      {
        apiVersion = "networking.gke.io/v1"
        kind       = "GCPBackendPolicy"
        metadata   = { name = "argocd-server", namespace = local.argocd_namespace }
        spec = {
          default = {
            timeoutSec     = 3600
            securityPolicy = local.platform.cloud_armor_policy_name
          }
          targetRef = { group = "", kind = "Service", name = "argocd-server" }
        }
      },
      local.argocd_repo_import_job,
    ]
  }
}

data "kubernetes_resource" "api_server" {
  api_version = "discovery.k8s.io/v1"
  kind        = "EndpointSlice"
  metadata {
    name      = "kubernetes"
    namespace = "default"
  }
}

resource "kubernetes_namespace_v1" "argocd" {
  metadata {
    name   = local.argocd_namespace
    labels = local.gateway_label
  }
}

resource "helm_release" "argocd" {
  name       = "argocd"
  repository = "https://argoproj.github.io/argo-helm"
  chart      = "argo-cd"
  version    = "10.10.3"
  namespace  = kubernetes_namespace_v1.argocd.metadata[0].name
  values     = [yamlencode(local.argocd_values)]
  timeout    = 900

  # The repository import hook needs its identity, permissions, secret mount and kustomization in place.
  depends_on = [
    kubernetes_network_policy_v1.argocd,
    google_secret_manager_secret_iam_member.argocd_repo_import,
    kubernetes_role_binding_v1.argocd_repo_import,
    kubernetes_manifest.argocd_repo_ssh_key,
    kubernetes_config_map_v1.argocd_repo_import,
  ]
}

# Deny by default. In: the namespace's own pods, and the load balancer to the server. Out: the namespace's own pods,
# DNS, the API server, GitHub (SSH, HTTPS) and the Gateway address, through which the infra realm is reached.
resource "kubernetes_network_policy_v1" "argocd" {
  metadata {
    name      = "argocd"
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }

  spec {
    pod_selector {}
    policy_types = ["Ingress", "Egress"]

    ingress {
      from {
        pod_selector {}
      }
    }

    ingress {
      dynamic "from" {
        for_each = local.gfe_ranges
        content {
          ip_block {
            cidr = from.value
          }
        }
      }
      ports {
        protocol = "TCP"
        port     = "8080"
      }
    }

    egress {
      to {
        pod_selector {}
      }
    }

    egress {
      to {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = "kube-system" }
        }
        pod_selector {
          match_labels = { "k8s-app" = "kube-dns" }
        }
      }
      to {
        ip_block {
          cidr = "169.254.20.10/32"
        }
      }
      ports {
        protocol = "UDP"
        port     = "53"
      }
      ports {
        protocol = "TCP"
        port     = "53"
      }
    }

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

    egress {
      dynamic "to" {
        for_each = local.github_ranges
        content {
          ip_block {
            cidr = to.value
          }
        }
      }
      ports {
        protocol = "TCP"
        port     = "22"
      }
      ports {
        protocol = "TCP"
        port     = "443"
      }
    }

    egress {
      to {
        ip_block {
          cidr = "${local.platform.gateway_ip_address}/32"
        }
      }
      ports {
        protocol = "TCP"
        port     = "443"
      }
    }
  }
}

# ── Repository access: a read-only deploy key, held in Secret Manager and copied into Argo CD's repository Secret by
# the Job below, so neither git nor Terraform state carries it (README.md: Argo CD) ─────────────────────────
resource "google_secret_manager_secret" "argocd_repo_ssh_key" {
  secret_id = "argocd-repo-ssh-key"

  replication {
    user_managed {
      replicas {
        location = local.region
      }
    }
  }
}

resource "google_secret_manager_secret_iam_member" "argocd_repo_import" {
  project   = local.project
  secret_id = google_secret_manager_secret.argocd_repo_ssh_key.id
  role      = "roles/secretmanager.secretAccessor"
  member    = local.argocd_repo_import_principal
}

resource "kubernetes_service_account_v1" "argocd_repo_import" {
  metadata {
    name      = local.argocd_repo_import
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }
}

# Create cannot be limited by name in RBAC; reading and changing are limited to the one repository Secret.
resource "kubernetes_role_v1" "argocd_repo_import" {
  metadata {
    name      = local.argocd_repo_import
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }

  rule {
    api_groups = [""]
    resources  = ["secrets"]
    verbs      = ["create"]
  }

  rule {
    api_groups     = [""]
    resources      = ["secrets"]
    resource_names = ["repo-conde"]
    verbs          = ["get", "patch"]
  }
}

resource "kubernetes_role_binding_v1" "argocd_repo_import" {
  metadata {
    name      = local.argocd_repo_import
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }

  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "Role"
    name      = kubernetes_role_v1.argocd_repo_import.metadata[0].name
  }

  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.argocd_repo_import.metadata[0].name
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }
}

resource "kubernetes_manifest" "argocd_repo_ssh_key" {
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = local.argocd_repo_import
      namespace = kubernetes_namespace_v1.argocd.metadata[0].name
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([{
          resourceName = "${google_secret_manager_secret.argocd_repo_ssh_key.id}/versions/latest"
          path         = "ssh-private-key"
        }])
      }
    }
  }
}

# kustomize builds the Secret from the mounted file; the kubectl image has no shell to template it with.
resource "kubernetes_config_map_v1" "argocd_repo_import" {
  metadata {
    name      = local.argocd_repo_import
    namespace = kubernetes_namespace_v1.argocd.metadata[0].name
  }

  data = {
    "kustomization.yaml" = yamlencode({
      apiVersion = "kustomize.config.k8s.io/v1beta1"
      kind       = "Kustomization"
      namespace  = local.argocd_namespace
      generatorOptions = {
        disableNameSuffixHash = true
        labels                = { "argocd.argoproj.io/secret-type" = "repository" }
      }
      secretGenerator = [{
        name     = "repo-conde"
        files    = ["sshPrivateKey=key/ssh-private-key"]
        literals = ["type=git", "name=conde", "url=${local.argocd_repo_url}"]
      }]
    })
  }
}

# ── The workspaces: one Application per deploy/tenants/<name>.yaml (the CRDs come with helm_release.argocd) ─────────
resource "kubernetes_manifest" "argocd_project" {
  manifest   = yamldecode(file("${path.module}/../../../deploy/argocd/project.yaml"))
  depends_on = [helm_release.argocd]
}

resource "kubernetes_manifest" "argocd_workspaces" {
  manifest = yamldecode(templatefile("${path.module}/../../../deploy/argocd/workspaces.yaml.tftpl", {
    repo_url        = local.argocd_repo_url
    revision        = var.gitops_revision
    platform_values = jsonencode(local.workspace_platform_values)
    domain          = local.workspace_domain
    project         = local.project
    postgres_image  = local.postgres_image
    pg_admin_secret = "pg-admin-password"
    # The same shape either way: the provider cannot drop the automated block in place.
    sync_policy = jsonencode({
      automated = { enabled = var.workspaces_auto_sync, selfHeal = true, prune = false }
    })
  }))

  depends_on = [kubernetes_manifest.argocd_project]
}
