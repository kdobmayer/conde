# Numeric GitHub ids (api.github.com/repos/<owner>/<repo>: owner.id and id); the provider exists only when both are set.
variable "github_repository_owner_id" {
  type    = string
  default = ""

  validation {
    condition     = can(regex("^[0-9]*$", var.github_repository_owner_id))
    error_message = "github_repository_owner_id is the numeric owner id."
  }
}

variable "github_repository_id" {
  type    = string
  default = ""

  validation {
    condition     = can(regex("^[0-9]*$", var.github_repository_id))
    error_message = "github_repository_id is the numeric repository id."
  }
}
