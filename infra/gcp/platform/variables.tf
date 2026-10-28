# Bump to rotate: one apply writes a new secret version and sets it on the database.
variable "pg_admin_password_version" {
  type    = number
  default = 1
}

variable "keycloak_db_password_version" {
  type    = number
  default = 1
}
