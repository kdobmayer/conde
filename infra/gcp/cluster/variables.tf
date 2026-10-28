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

# Off for the first adoption of the Terraform-created objects; on after it (selfHeal on, prune off).
variable "workspaces_auto_sync" {
  type    = bool
  default = false
}
