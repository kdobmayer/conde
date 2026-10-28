variable "require_mfa" {
  type        = map(bool)
  description = "Per tenant realm: make Configure OTP a default required action (every new user enrols at first sign-in)."
  default = {
    demo       = false
    clearscale = false
  }
}

# The Keycloak the provider calls. run.sh sets http://127.0.0.1:18080, a port-forward to the Keycloak Service; the public
# URL serves the admin paths only to admin_cidrs, and only until the allowlist is retired (docs/infra-gcp-s4-iap.md).
variable "provider_url" {
  type    = string
  default = "https://auth.conde.cloud"
}
