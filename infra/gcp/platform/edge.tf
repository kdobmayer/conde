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

resource "google_compute_security_policy" "web" {
  name        = "conde-web"
  description = "conde Gateway backends: login rate ban, no Keycloak admin paths, default allow"

  adaptive_protection_config {
    layer_7_ddos_defense_config {
      enable = true
    }
  }

  # The Keycloak console and the master realm are served only on admin-auth.conde.cloud, behind IAP, whose backend has
  # its own policy; the public routes do not carry these paths, and these rules refuse them should a path ever reach
  # Keycloak through a public route.
  rule {
    priority    = 1000
    action      = "deny(403)"
    description = "No Keycloak admin console or master realm on the public backends"
    match {
      expr {
        expression = "request.path.matches('^/admin/|^/realms/master/')"
      }
    }
  }

  # The bare paths (no trailing slash) are a second rule: the regex above cannot say "end of path" without a group.
  rule {
    priority    = 1001
    action      = "deny(403)"
    description = "No bare /admin or /realms/master on the public backends"
    match {
      expr {
        expression = "request.path == '/admin' || request.path == '/realms/master'"
      }
    }
  }

  # Keycloak normalises percent-encoded dot segments (`/realms/demo/%2e%2e/master/...` serves the master realm). Cloud
  # Armor evaluates the normalised path, so encoded input does not match here; the canonical-path denies 1000 and 1001
  # refuse the admin and master paths after normalisation. Defence in depth only, should that evaluation change.
  rule {
    priority    = 1002
    action      = "deny(403)"
    description = "Defence in depth only: the admin and master paths are protected by the canonical-path denies after normalisation"
    match {
      expr {
        expression = "request.headers['host'].lower().startsWith('auth.conde.cloud') && (request.path.contains('%') || request.path.contains('..'))"
      }
    }
  }

  rule {
    priority    = 2000
    action      = "rate_based_ban"
    description = "Sign-in paths: over 100 requests a minute from one IP bans it for 10 minutes"
    match {
      expr {
        expression = "request.path.matches('^/auth/|^/realms/[^/]+/login-actions/|^/realms/[^/]+/protocol/openid-connect/token')"
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

# The admin backends (Keycloak console on admin-auth.conde.cloud, Argo CD) sit behind Identity-Aware Proxy; Cloud Armor,
# evaluated before IAP, only throttles each client IP there. A UI page load is tens of requests.
locals {
  admin_policies = {
    keycloak-admin = { name = "conde-keycloak-admin", description = "Keycloak admin console backend (behind IAP): per-IP throttle", per_minute = 300 }
    argocd         = { name = "conde-argocd", description = "Argo CD backend (behind IAP): per-IP throttle", per_minute = 600 }
  }
}

resource "google_compute_security_policy" "admin" {
  for_each = local.admin_policies

  name        = each.value.name
  description = each.value.description

  rule {
    priority    = 1000
    action      = "throttle"
    description = "Over ${each.value.per_minute} requests a minute from one IP get 429"
    match {
      versioned_expr = "SRC_IPS_V1"
      config {
        src_ip_ranges = ["*"]
      }
    }
    rate_limit_options {
      conform_action = "allow"
      exceed_action  = "deny(429)"
      enforce_on_key = "IP"
      rate_limit_threshold {
        count        = each.value.per_minute
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
