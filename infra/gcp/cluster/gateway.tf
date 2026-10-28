# ── The shared Gateway: one global external Application Load Balancer for every hostname under conde.cloud ──────
resource "kubernetes_namespace_v1" "gateway" {
  metadata {
    name = "gateway"
  }
}

resource "kubernetes_manifest" "gateway" {
  manifest = {
    apiVersion = "gateway.networking.k8s.io/v1"
    kind       = "Gateway"
    metadata = {
      name      = "conde"
      namespace = kubernetes_namespace_v1.gateway.metadata[0].name
      annotations = {
        "networking.gke.io/certmap" = local.platform.certificate_map_name
      }
    }
    spec = {
      gatewayClassName = "gke-l7-global-external-managed"
      addresses = [{
        type  = "NamedAddress"
        value = local.platform.gateway_ip_name
      }]
      listeners = [
        {
          name     = "https"
          protocol = "HTTPS"
          port     = 443
          allowedRoutes = {
            namespaces = {
              from     = "Selector"
              selector = { matchLabels = local.gateway_label }
            }
          }
        },
        {
          name     = "http"
          protocol = "HTTP"
          port     = 80
          allowedRoutes = {
            namespaces = { from = "Same" }
          }
        },
      ]
    }
  }
}

resource "kubernetes_manifest" "http_redirect" {
  manifest = {
    apiVersion = "gateway.networking.k8s.io/v1"
    kind       = "HTTPRoute"
    metadata = {
      name      = "https-redirect"
      namespace = kubernetes_namespace_v1.gateway.metadata[0].name
    }
    spec = {
      parentRefs = [{
        name        = kubernetes_manifest.gateway.manifest.metadata.name
        sectionName = "http"
      }]
      rules = [{
        filters = [{
          type = "RequestRedirect"
          requestRedirect = {
            scheme     = "https"
            statusCode = 301
          }
        }]
      }]
    }
  }
}
