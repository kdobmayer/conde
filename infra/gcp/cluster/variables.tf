# Argo CD's local admin; set false once sign-in through the infra realm is verified (README: Argo CD).
variable "argocd_local_admin" {
  type    = bool
  default = true
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
