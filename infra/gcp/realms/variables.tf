variable "require_mfa" {
  type        = map(bool)
  description = "Per tenant realm: make Configure OTP a default required action (every new user enrols at first sign-in)."
  default = {
    demo       = false
    clearscale = false
  }
}
