image        = "crnative.azurecr.io/conde@sha256:4490a8e749eb2d48edae1a6aa5aaa392f8f2e5f5c4dd554a12d016810cd748c9"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
