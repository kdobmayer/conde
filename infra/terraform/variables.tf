variable "image" {
  type        = string
  description = "Image of the conde server; Microsoft's quickstart until the first conde image is in crnative."
  default     = "mcr.microsoft.com/k8se/quickstart:latest"
}

variable "target_port" {
  type        = number
  description = "Port the image listens on: 80 for the quickstart, 8080 for conde (set both together, conde.auto.tfvars)."
  default     = 80
}

variable "replicas" {
  type    = number
  default = 1
}

# Bump to rotate; one apply rewrites the server password and the connection URL from the vault.
variable "db_password_version" {
  type    = number
  default = 1
}

variable "admin_key_version" {
  type    = number
  default = 1
}

# Query string of DATABASE_URL (TLS settings the app's driver understands); a change here is a new secret version
# without touching the password.
variable "database_url_params" {
  type    = string
  default = "sslmode=verify-full&sslrootcert=/etc/ssl/certs/ca-certificates.crt"
}

variable "database_url_revision" {
  type    = number
  default = 1
}

variable "board_name" {
  type        = string
  description = "Name of this board as the v2 protocol announces it to hosts."
  default     = "dakai"
}

variable "min_proto" {
  type        = number
  description = "Lowest protocol version a host may speak: 1 keeps v1 hosts working until every teammate upgrades; 2 afterwards."
  default     = 1
}

# "owner=U…" pairs (comma-separated) the Slack worker DMs when a message is addressed to that owner; empty = off.
variable "slack_notify" {
  type    = string
  default = ""
}
