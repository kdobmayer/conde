# Argo CD's local admin; off since sign-in through the infra realm is verified (README: Argo CD).
variable "argocd_local_admin" {
  type    = bool
  default = false
}

variable "argocd_oidc_issuer" {
  type    = string
  default = "https://auth.conde.cloud/realms/infra"
}

variable "argocd_oidc_client_id" {
  type    = string
  default = "argocd"
}

variable "argocd_admin_group" {
  type        = string
  default     = "infra-admins"
  description = "The groups-claim value that is Argo CD role:admin; nobody else gets any access."
}

variable "gitops_revision" {
  type        = string
  default     = "main"
  description = "Branch of github.com/kdobmayer/conde that Argo CD reads charts/ and deploy/ from."
}

# Automated sync with self-heal, without pruning; false pauses it for every workspace (as for a database restore).
variable "workspaces_auto_sync" {
  type    = bool
  default = true
}

# Bump after adding a new version of the argocd-repo-ssh-key secret: the Argo CD release upgrades and its hook re-imports
# the key (README: Repository access).
variable "argocd_repo_key_version" {
  type    = number
  default = 1
}

# The client id of the OAuth client `iap-admin` (docs/infra-gcp-s4-iap.md); not a secret.
variable "iap_client_id" {
  type    = string
  default = "327673226052-0uppqjn1ujh4df0alpv2oagplaa099g2.apps.googleusercontent.com"

  validation {
    condition     = endswith(var.iap_client_id, ".apps.googleusercontent.com")
    error_message = "iap_client_id is an OAuth client id (<number>-<id>.apps.googleusercontent.com)."
  }
}

# The Secret Manager version of iap-admin-client-secret written into keycloak/iap-admin and argocd/iap-admin. After adding
# a version, set its number here and apply; a new "latest" alone changes nothing.
variable "iap_client_secret_version" {
  type    = number
  default = 1

  validation {
    condition     = var.iap_client_secret_version >= 1 && floor(var.iap_client_secret_version) == var.iap_client_secret_version
    error_message = "iap_client_secret_version is a Secret Manager version number (1, 2, ...)."
  }
}
