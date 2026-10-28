# ── Public entry: static IP for the Gateway, Certificate Manager wildcard certificate, Cloud Armor policy ───────
resource "google_compute_global_address" "gateway" {
  name = "conde-gateway"
}

resource "google_certificate_manager_dns_authorization" "conde" {
  name   = "conde-cloud"
  domain = local.domain
}

resource "google_certificate_manager_certificate" "conde" {
  name = "conde-cloud"

  managed {
    domains            = [local.domain, "*.${local.domain}"]
    dns_authorizations = [google_certificate_manager_dns_authorization.conde.id]
  }
}

resource "google_certificate_manager_certificate_map" "conde" {
  name = "conde"
}

resource "google_certificate_manager_certificate_map_entry" "apex" {
  name         = "conde-cloud"
  map          = google_certificate_manager_certificate_map.conde.name
  hostname     = local.domain
  certificates = [google_certificate_manager_certificate.conde.id]
}

resource "google_certificate_manager_certificate_map_entry" "wildcard" {
  name         = "wildcard-conde-cloud"
  map          = google_certificate_manager_certificate_map.conde.name
  hostname     = "*.${local.domain}"
  certificates = [google_certificate_manager_certificate.conde.id]
}

locals {
  # Cloud Armor allows at most five subexpressions per rule (one path match plus up to four ranges) and no regex groups.
  admin_from_allowlist = join(" || ", [for c in var.admin_cidrs : "inIpRange(origin.ip, '${c}')"])
}

resource "google_compute_security_policy" "web" {
  name        = "conde-web"
  description = "conde Gateway backends: login rate ban, admin paths from the allowlist only, default allow"

  adaptive_protection_config {
    layer_7_ddos_defense_config {
      enable = true
    }
  }

  rule {
    priority    = 1000
    action      = "deny(403)"
    description = "Keycloak admin console and master realm only from admin_cidrs"
    match {
      expr {
        expression = "request.path.matches('^/admin/|^/realms/master/') && !(${local.admin_from_allowlist})"
      }
    }
  }

  # The bare paths (no trailing slash) are a second rule: the regex above cannot say "end of path" without a group.
  rule {
    priority    = 1001
    action      = "deny(403)"
    description = "Bare /admin and /realms/master only from admin_cidrs"
    match {
      expr {
        expression = "(request.path == '/admin' || request.path == '/realms/master') && !(${local.admin_from_allowlist})"
      }
    }
  }

  # Percent-encoded admin paths are refused outright; no Keycloak endpoint we route has an encoded path.
  rule {
    priority    = 1002
    action      = "deny(403)"
    description = "No percent-encoded paths toward the admin console"
    match {
      expr {
        expression = "request.path.lower().contains('%2f') || request.path.lower().contains('%61dmin')"
      }
    }
  }

  # The whole Argo CD host (UI, API, gRPC); lower() and startsWith also catch an upper-case or host:port header.
  rule {
    priority    = 1003
    action      = "deny(403)"
    description = "Argo CD host only from admin_cidrs"
    match {
      expr {
        expression = "request.headers['host'].lower().startsWith('argocd.conde.cloud') && !(${local.admin_from_allowlist})"
      }
    }
  }

  rule {
    priority    = 2000
    action      = "rate_based_ban"
    description = "Sign-in paths: over 100 requests a minute from one IP bans it for 10 minutes"
    match {
      expr {
        expression = "request.path.matches('^/auth/')"
      }
    }
    rate_limit_options {
      conform_action   = "allow"
      exceed_action    = "deny(429)"
      enforce_on_key   = "IP"
      ban_duration_sec = 600
      rate_limit_threshold {
        count        = 100
        interval_sec = 60
      }
      ban_threshold {
        count        = 100
        interval_sec = 60
      }
    }
  }

  rule {
    priority    = 2147483647
    action      = "allow"
    description = "Default allow"
    match {
      versioned_expr = "SRC_IPS_V1"
      config {
        src_ip_ranges = ["*"]
      }
    }
  }
}
