# ── Keycloak: one installation, realms per workspace; database `keycloak` on conde-pg through the Auth Proxy ──────
locals {
  keycloak_labels = { "app.kubernetes.io/name" = "keycloak" }
  keycloak_secrets = {
    "db-password" = "keycloak-db-password"
  }
}

resource "kubernetes_namespace_v1" "keycloak" {
  metadata {
    name   = "keycloak"
    labels = local.gateway_label
  }
}

resource "google_service_account" "keycloak" {
  account_id   = "keycloak"
  display_name = "Keycloak (Cloud SQL Auth Proxy)"
}

resource "google_project_iam_member" "keycloak_sql_client" {
  project = local.project
  role    = "roles/cloudsql.client"
  member  = google_service_account.keycloak.member
}

resource "kubernetes_service_account_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    annotations = {
      "iam.gke.io/gcp-service-account" = google_service_account.keycloak.email
    }
  }
}

resource "google_service_account_iam_member" "keycloak_workload_identity" {
  service_account_id = google_service_account.keycloak.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${local.wi_pool}[${kubernetes_namespace_v1.keycloak.metadata[0].name}/${kubernetes_service_account_v1.keycloak.metadata[0].name}]"
}

# The bootstrap admin is gone (realms/README.md); its secret leaves this state but stays in Secret Manager until deleted.
removed {
  from = google_secret_manager_secret.keycloak_bootstrap_admin
  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.keycloak_bootstrap_admin
  lifecycle {
    destroy = false
  }
}

moved {
  from = google_secret_manager_secret_iam_member.keycloak["bootstrap-admin-password"]
  to   = google_secret_manager_secret_iam_member.keycloak_bootstrap_admin
}

removed {
  from = google_secret_manager_secret_iam_member.keycloak_bootstrap_admin
  lifecycle {
    destroy = false
  }
}

resource "google_secret_manager_secret_iam_member" "keycloak" {
  for_each = local.keycloak_secrets

  project   = local.project
  secret_id = "projects/${local.project}/secrets/${each.value}"
  role      = "roles/secretmanager.secretAccessor"
  # The CSI add-on reads as the Google service account the pod's KSA is annotated with.
  member = google_service_account.keycloak.member
}

resource "kubernetes_manifest" "keycloak_secrets" {
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = "keycloak"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([for path, secret in local.keycloak_secrets : {
          resourceName = "projects/${local.project}/secrets/${secret}/versions/latest"
          path         = path
        }])
      }
    }
  }
}

# Keycloak's file vault: a realm's `${vault.<key>}` reads the file `<realm>_<key>` (underscores in either doubled), so
# these values stay out of the realms state. One Secret Manager secret can back several files.
locals {
  keycloak_vault = merge(
    { for realm in ["demo", "clearscale", "infra", "master"] : "${realm}_smtp-password" => "resend-api-key" },
    { "clearscale_google-client-secret" = "clearscale-google-oauth-client-secret" },
  )
}

resource "google_secret_manager_secret_iam_member" "keycloak_vault" {
  for_each = toset(values(local.keycloak_vault))

  project   = local.project
  secret_id = "projects/${local.project}/secrets/${each.value}"
  role      = "roles/secretmanager.secretAccessor"
  member    = google_service_account.keycloak.member
}

resource "kubernetes_manifest" "keycloak_vault" {
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = "keycloak-vault"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([for path, secret in local.keycloak_vault : {
          resourceName = "projects/${local.project}/secrets/${secret}/versions/latest"
          path         = path
        }])
      }
    }
  }
}

resource "kubernetes_deployment_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    labels    = local.keycloak_labels
  }

  spec {
    replicas = 1

    selector {
      match_labels = local.keycloak_labels
    }

    strategy {
      type = "RollingUpdate"
      rolling_update {
        max_surge       = "1"
        max_unavailable = "0"
      }
    }

    template {
      metadata {
        labels = local.keycloak_labels
      }

      spec {
        service_account_name             = kubernetes_service_account_v1.keycloak.metadata[0].name
        termination_grace_period_seconds = 60

        # Autopilot adds it at admission; stated here so plans stay empty.
        toleration {
          key      = "kubernetes.io/arch"
          operator = "Equal"
          value    = "amd64"
          effect   = "NoSchedule"
        }

        security_context {
          run_as_non_root = true
          seccomp_profile {
            type = "RuntimeDefault"
          }
        }

        # Native sidecar: Keycloak starts only after the proxy's listener is up.
        init_container {
          name           = "cloud-sql-proxy"
          image          = local.cloud_sql_proxy
          restart_policy = "Always"
          args = [
            "--psc",
            "--structured-logs",
            "--address=127.0.0.1",
            "--port=5432",
            "--health-check",
            "--http-address=0.0.0.0",
            "--http-port=9090",
            local.platform.sql_connection_name,
          ]

          startup_probe {
            http_get {
              path = "/startup"
              port = 9090
            }
            period_seconds    = 2
            failure_threshold = 60
          }

          resources {
            requests = {
              cpu                 = "50m"
              memory              = "64Mi"
              "ephemeral-storage" = "100Mi"
            }
            # Autopilot sets the ephemeral-storage limit to the request; stated here so plans stay empty.
            limits = {
              "ephemeral-storage" = "100Mi"
            }
          }

          security_context {
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }
        }

        container {
          name    = "keycloak"
          image   = local.keycloak_image
          command = ["/bin/bash", "-c"]
          # Keycloak has no *_FILE convention: the CSI-mounted files become its environment, then kc.sh replaces the shell.
          args = [<<-EOT
            set -e
            KC_DB_PASSWORD=$(< /secrets/db-password)
            export KC_DB_PASSWORD
            exec /opt/keycloak/bin/kc.sh start --optimized
          EOT
          ]

          dynamic "env" {
            for_each = {
              KC_HOSTNAME             = "https://${local.keycloak_hostname}"
              KC_HOSTNAME_ADMIN       = "https://${local.keycloak_admin_hostname}"
              KC_PROXY_HEADERS        = "xforwarded"
              KC_HTTP_ENABLED         = "true"
              KC_HTTP_PORT            = "8080"
              KC_HTTP_MANAGEMENT_PORT = "9000"
              KC_CACHE                = "ispn"
              KC_CACHE_STACK          = "jdbc-ping"
              KC_DB_URL               = "jdbc:postgresql://127.0.0.1:5432/keycloak"
              KC_DB_USERNAME          = "keycloak"
              KC_VAULT_DIR            = "/vault"
            }
            content {
              name  = env.key
              value = env.value
            }
          }

          port {
            name           = "http"
            container_port = 8080
          }
          port {
            name           = "management"
            container_port = 9000
          }

          startup_probe {
            http_get {
              path = "/health/started"
              port = 9000
            }
            timeout_seconds   = 5
            period_seconds    = 5
            failure_threshold = 60
          }

          readiness_probe {
            http_get {
              path = "/health/ready"
              port = 9000
            }
            timeout_seconds   = 5
            period_seconds    = 10
            failure_threshold = 3
          }

          liveness_probe {
            http_get {
              path = "/health/live"
              port = 9000
            }
            timeout_seconds   = 5
            period_seconds    = 10
            failure_threshold = 3
          }

          resources {
            requests = {
              cpu                 = "250m"
              memory              = "768Mi"
              "ephemeral-storage" = "1Gi"
            }
            limits = {
              memory              = "1536Mi"
              "ephemeral-storage" = "1Gi"
            }
          }

          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }

          volume_mount {
            name       = "secrets"
            mount_path = "/secrets"
            read_only  = true
          }

          volume_mount {
            name       = "vault"
            mount_path = "/vault"
            read_only  = true
          }
        }

        volume {
          name = "secrets"
          csi {
            driver    = "secrets-store-gke.csi.k8s.io"
            read_only = true
            volume_attributes = {
              secretProviderClass = kubernetes_manifest.keycloak_secrets.manifest.metadata.name
            }
          }
        }

        volume {
          name = "vault"
          csi {
            driver    = "secrets-store-gke.csi.k8s.io"
            read_only = true
            volume_attributes = {
              secretProviderClass = kubernetes_manifest.keycloak_vault.manifest.metadata.name
            }
          }
        }
      }
    }
  }

  timeouts {
    create = "15m"
    update = "15m"
  }

  # Autopilot records its resource adjustments in annotations.
  lifecycle {
    ignore_changes = [metadata[0].annotations]
  }

  depends_on = [
    google_project_iam_member.keycloak_sql_client,
    google_service_account_iam_member.keycloak_workload_identity,
    google_secret_manager_secret_iam_member.keycloak,
    google_secret_manager_secret_iam_member.keycloak_vault,
  ]
}

resource "kubernetes_service_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
  }

  # The NEG controller annotates the Service.
  lifecycle {
    ignore_changes = [metadata[0].annotations]
  }

  spec {
    type     = "ClusterIP"
    selector = local.keycloak_labels

    port {
      name        = "http"
      port        = 8080
      target_port = "http"
    }
  }
}

# Only the workspace realms, the infra realm (Argo CD sign-in) and static resources; the console and the master realm are
# on admin-auth.conde.cloud (keycloak_admin_route).
resource "kubernetes_manifest" "keycloak_route" {
  manifest = {
    apiVersion = "gateway.networking.k8s.io/v1"
    kind       = "HTTPRoute"
    metadata = {
      name      = "keycloak"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      parentRefs = [{
        name        = kubernetes_manifest.gateway.manifest.metadata.name
        namespace   = kubernetes_namespace_v1.gateway.metadata[0].name
        sectionName = "https"
      }]
      hostnames = [local.keycloak_hostname]
      rules = [{
        matches = [for prefix in ["/realms/demo", "/realms/clearscale", "/realms/infra", "/resources"] : {
          path = { type = "PathPrefix", value = prefix }
        }]
        backendRefs = [{
          name = kubernetes_service_v1.keycloak.metadata[0].name
          port = 8080
        }]
      }]
    }
  }
}

# The admin console host: its own Service, so the load balancer gives it its own backend service, which IAP guards
# (iap.tf). The console needs the master realm (its own sign-in: the realm's frontend URL is this host, realms root)
# and the theme resources; it calls the admin API on KC_HOSTNAME_ADMIN. /_gcp_gatekeeper is IAP's sign-in return
# path, kept on the IAP backend so the load balancer never sends it to the 404 default.
resource "kubernetes_service_v1" "keycloak_admin" {
  metadata {
    name      = "keycloak-admin"
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
  }

  lifecycle {
    ignore_changes = [metadata[0].annotations]
  }

  spec {
    type     = "ClusterIP"
    selector = local.keycloak_labels

    port {
      name        = "http"
      port        = 8080
      target_port = "http"
    }
  }
}

resource "kubernetes_manifest" "keycloak_admin_route" {
  manifest = {
    apiVersion = "gateway.networking.k8s.io/v1"
    kind       = "HTTPRoute"
    metadata = {
      name      = "keycloak-admin"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      parentRefs = [{
        name        = kubernetes_manifest.gateway.manifest.metadata.name
        namespace   = kubernetes_namespace_v1.gateway.metadata[0].name
        sectionName = "https"
      }]
      hostnames = [local.keycloak_admin_hostname]
      rules = [{
        matches = [for prefix in ["/admin", "/realms/master", "/resources", "/_gcp_gatekeeper"] : {
          path = { type = "PathPrefix", value = prefix }
        }]
        backendRefs = [{
          name = kubernetes_service_v1.keycloak_admin.metadata[0].name
          port = 8080
        }]
      }]
    }
  }
}

resource "kubernetes_manifest" "keycloak_admin_health_check" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "HealthCheckPolicy"
    metadata = {
      name      = "keycloak-admin"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      default = {
        checkIntervalSec   = 10
        timeoutSec         = 5
        healthyThreshold   = 1
        unhealthyThreshold = 3
        config = {
          type = "HTTP"
          httpHealthCheck = {
            port        = 9000
            requestPath = "/health/ready"
          }
        }
      }
      targetRef = {
        group = ""
        kind  = "Service"
        name  = kubernetes_service_v1.keycloak_admin.metadata[0].name
      }
    }
  }
}

resource "kubernetes_manifest" "keycloak_admin_backend" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "GCPBackendPolicy"
    metadata = {
      name      = "keycloak-admin"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      default = {
        timeoutSec     = 60
        securityPolicy = local.platform.cloud_armor_admin_policy_names["keycloak-admin"]
        iap            = local.iap_policy
      }
      targetRef = {
        group = ""
        kind  = "Service"
        name  = kubernetes_service_v1.keycloak_admin.metadata[0].name
      }
    }
  }

  # The Secret it names (iap.tf).
  depends_on = [kubernetes_secret_v1.iap]
}

resource "kubernetes_manifest" "keycloak_health_check" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "HealthCheckPolicy"
    metadata = {
      name      = "keycloak"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      default = {
        checkIntervalSec   = 10
        timeoutSec         = 5
        healthyThreshold   = 1
        unhealthyThreshold = 3
        config = {
          type = "HTTP"
          httpHealthCheck = {
            port        = 9000
            requestPath = "/health/ready"
          }
        }
      }
      targetRef = {
        group = ""
        kind  = "Service"
        name  = kubernetes_service_v1.keycloak.metadata[0].name
      }
    }
  }
}

resource "kubernetes_manifest" "keycloak_backend" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "GCPBackendPolicy"
    metadata = {
      name      = "keycloak"
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      default = {
        timeoutSec     = 60
        securityPolicy = local.platform.cloud_armor_policy_name
      }
      targetRef = {
        group = ""
        kind  = "Service"
        name  = kubernetes_service_v1.keycloak.metadata[0].name
      }
    }
  }
}

# Deny by default; in from the load balancer and the demo workspace, out to DNS, the metadata server, the Cloud SQL
# PSC endpoint, public HTTPS (the Cloud SQL Admin API, the federated identity providers) and public SMTPS.
resource "kubernetes_network_policy_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
  }

  spec {
    pod_selector {}
    policy_types = ["Ingress", "Egress"]

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
      ports {
        protocol = "TCP"
        port     = "9000"
      }
    }

    # The demo workspace's server calls the admin API (user invites) on the Service, not through the load balancer.
    ingress {
      from {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = "ws-demo" }
        }
      }
      ports {
        protocol = "TCP"
        port     = "8080"
      }
    }

    # JGroups (jdbc-ping discovery) between Keycloak pods once there is more than one.
    ingress {
      from {
        pod_selector {
          match_labels = local.keycloak_labels
        }
      }
      ports {
        protocol = "TCP"
        port     = "7800"
      }
      ports {
        protocol = "TCP"
        port     = "57800"
      }
    }

    egress {
      to {
        pod_selector {
          match_labels = local.keycloak_labels
        }
      }
      ports {
        protocol = "TCP"
        port     = "7800"
      }
      ports {
        protocol = "TCP"
        port     = "57800"
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
      ports {
        protocol = "UDP"
        port     = "53"
      }
      ports {
        protocol = "TCP"
        port     = "53"
      }
    }

    dynamic "egress" {
      for_each = local.network_policy_egress
      content {
        to {
          ip_block {
            cidr   = egress.value.cidr
            except = egress.value.except
          }
        }
        dynamic "ports" {
          for_each = egress.value.ports
          content {
            protocol = ports.value.protocol
            port     = ports.value.port
          }
        }
      }
    }

    # SMTP submission with implicit TLS (smtp.resend.com) for the realms' emails; Keycloak's policy only.
    egress {
      to {
        ip_block {
          cidr   = "0.0.0.0/0"
          except = local.private_ranges
        }
      }
      ports {
        protocol = "TCP"
        port     = "465"
      }
    }
  }
}

# ── Database grants: Keycloak owns its schema, no other role reaches its database, `postgres` is closed to PUBLIC ──
locals {
  keycloak_dbinit = "keycloak-dbinit"
  # Without a GSA annotation: the instance admin password is readable by this Job's principal only.
  keycloak_dbinit_principal = "principal://iam.googleapis.com/projects/${local.foundation.project_number}/locations/global/workloadIdentityPools/${local.wi_pool}/subject/ns/${kubernetes_namespace_v1.keycloak.metadata[0].name}/sa/${local.keycloak_dbinit}"
}

resource "kubernetes_service_account_v1" "keycloak_dbinit" {
  metadata {
    name      = local.keycloak_dbinit
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
  }
}

resource "google_project_iam_member" "keycloak_dbinit_sql" {
  project = local.project
  role    = "roles/cloudsql.client"
  member  = local.keycloak_dbinit_principal
}

resource "google_secret_manager_secret_iam_member" "keycloak_dbinit" {
  project   = local.project
  secret_id = "projects/${local.project}/secrets/pg-admin-password"
  role      = "roles/secretmanager.secretAccessor"
  member    = local.keycloak_dbinit_principal
}

resource "kubernetes_manifest" "keycloak_dbinit_secrets" {
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = local.keycloak_dbinit
      namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([{
          resourceName = "projects/${local.project}/secrets/pg-admin-password/versions/latest"
          path         = "pg-admin-password"
        }])
      }
    }
  }
}

# As the instance's admin user, idempotent. Cloud SQL makes every built-in user a member of cloudsqlsuperuser; once
# Keycloak owns the public schema (it already owns every table) it needs neither that membership nor PUBLIC's grants.
resource "kubernetes_job_v1" "keycloak_dbinit" {
  metadata {
    name      = local.keycloak_dbinit
    namespace = kubernetes_namespace_v1.keycloak.metadata[0].name
  }

  spec {
    backoff_limit = 6

    template {
      metadata {
        labels = { "app.kubernetes.io/name" = local.keycloak_dbinit }
      }

      spec {
        service_account_name = kubernetes_service_account_v1.keycloak_dbinit.metadata[0].name
        restart_policy       = "Never"

        toleration {
          key      = "kubernetes.io/arch"
          operator = "Equal"
          value    = "amd64"
          effect   = "NoSchedule"
        }

        security_context {
          run_as_non_root = true
          seccomp_profile {
            type = "RuntimeDefault"
          }
        }

        init_container {
          name           = "cloud-sql-proxy"
          image          = local.cloud_sql_proxy
          restart_policy = "Always"
          args = [
            "--psc",
            "--structured-logs",
            "--address=127.0.0.1",
            "--port=5432",
            "--health-check",
            "--http-address=0.0.0.0",
            "--http-port=9090",
            local.platform.sql_connection_name,
          ]

          startup_probe {
            http_get {
              path = "/startup"
              port = 9090
            }
            period_seconds    = 2
            failure_threshold = 60
          }

          resources {
            requests = {
              cpu                 = "250m"
              memory              = "256Mi"
              "ephemeral-storage" = "100Mi"
            }
            limits = {
              "ephemeral-storage" = "100Mi"
            }
          }

          security_context {
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }
        }

        container {
          name    = "psql"
          image   = "docker.io/library/postgres:18.6-alpine@sha256:10a5c69a83035ab5239f5620d708520e8b28f5aa1f2b20143e6024532edadfb5"
          command = ["/bin/sh", "-c"]
          # The admin must be a member of the role it hands the schema to.
          args = [<<-EOT
            set -eu
            PGPASSWORD=$(cat /secrets/pg-admin-password)
            export PGPASSWORD
            psql -v ON_ERROR_STOP=1 -X -h 127.0.0.1 -U postgres -d postgres \
              -c 'GRANT keycloak TO postgres' \
              -c 'GRANT CONNECT, TEMPORARY ON DATABASE keycloak TO keycloak' \
              -c 'REVOKE CONNECT, TEMPORARY ON DATABASE keycloak FROM PUBLIC' \
              -c 'REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC'
            psql -v ON_ERROR_STOP=1 -X -h 127.0.0.1 -U postgres -d keycloak \
              -c 'ALTER SCHEMA public OWNER TO keycloak' \
              -c 'REVOKE cloudsqlsuperuser FROM keycloak'
          EOT
          ]

          resources {
            requests = {
              cpu                 = "250m"
              memory              = "256Mi"
              "ephemeral-storage" = "100Mi"
            }
            limits = {
              "ephemeral-storage" = "100Mi"
            }
          }

          security_context {
            run_as_user                = 70
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }

          volume_mount {
            name       = "secrets"
            mount_path = "/secrets"
            read_only  = true
          }
        }

        volume {
          name = "secrets"
          csi {
            driver    = "secrets-store-gke.csi.k8s.io"
            read_only = true
            volume_attributes = {
              secretProviderClass = kubernetes_manifest.keycloak_dbinit_secrets.manifest.metadata.name
            }
          }
        }
      }
    }
  }

  wait_for_completion = true

  timeouts {
    create = "10m"
    update = "10m"
  }

  depends_on = [
    google_project_iam_member.keycloak_dbinit_sql,
    google_secret_manager_secret_iam_member.keycloak_dbinit,
  ]
}
