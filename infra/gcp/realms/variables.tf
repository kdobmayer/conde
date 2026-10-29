variable "require_mfa" {
  type        = map(bool)
  description = "Per tenant realm: make Configure OTP a default required action (every new user enrols at first sign-in)."
  default = {
    demo       = false
    clearscale = false
    vvpusa     = false
  }
}

# The Keycloak the provider calls, set by run.sh: a port-forward to the Keycloak Service. The public host serves no
# admin path.
variable "provider_url" {
  type = string
}

# Bump to rotate the master realm's `terraform` client secret: one targeted apply sets a new secret on the client and
# writes it to keycloak-terraform-client-secret (README.md: Rotating the terraform client secret).
variable "terraform_client_secret_version" {
  type    = number
  default = 2
}
