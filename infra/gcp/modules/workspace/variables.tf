variable "name" {
  type        = string
  description = "Workspace name: the database conde_<name>, the namespace ws-<name>, the Keycloak realm <name>."

  validation {
    condition     = can(regex("^[a-z][a-z0-9]{1,19}$", var.name))
    error_message = "name is 2-20 lowercase letters and digits, starting with a letter."
  }
}

variable "hostname" {
  type = string
}

variable "oidc_issuer" {
  type        = string
  default     = null
  description = "OIDC issuer the server signs in against; null is the workspace's Keycloak realm."
}

variable "platform" {
  description = "Shared context from the foundation, platform and cluster roots."
  type = object({
    project             = string
    project_number      = string
    region              = string
    sql_connection_name = string
    keycloak_url        = string
    pg_admin_secret     = string
  })
}

variable "extra_secrets" {
  type        = list(string)
  default     = []
  description = "Further Secret Manager secrets its server reads, such as a Keycloak admin client secret."
}
