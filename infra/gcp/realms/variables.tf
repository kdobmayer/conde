variable "require_mfa" {
  type        = map(bool)
  description = "Per tenant realm: make Configure OTP a default required action (every new user enrols at first sign-in)."
  default = {
    demo       = false
    clearscale = false
  }
}

# TODO(owner): the Gmail account and its app password (host smtp.gmail.com, port 587); null leaves the realms without
# email, so invitations and email verification wait for it.
variable "smtp" {
  type = object({
    host     = string
    port     = optional(string, "587")
    from     = string
    username = string
  })
  default = null
}

# Given at run time (TF_VAR_smtp_password); Keycloak's realm resource keeps it in this state.
variable "smtp_password" {
  type      = string
  default   = ""
  sensitive = true
}
