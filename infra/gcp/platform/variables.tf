variable "admin_cidrs" {
  type        = list(string)
  description = "Source ranges allowed to reach the Keycloak admin console (/admin/, /realms/master/) and the Argo CD host."

  validation {
    condition     = length(var.admin_cidrs) >= 1 && length(var.admin_cidrs) <= 3 && alltrue([for c in var.admin_cidrs : can(cidrhost(c, 0))])
    error_message = "admin_cidrs holds one to four CIDR ranges."
  }
}

# Bump to rotate: one apply writes a new secret version and sets it on the database.
variable "pg_admin_password_version" {
  type    = number
  default = 1
}

variable "keycloak_db_password_version" {
  type    = number
  default = 1
}
