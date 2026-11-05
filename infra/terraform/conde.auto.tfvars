image        = "crnative.azurecr.io/conde@sha256:2f6f16898f0b4622e436a3fbd4e9f897d57750978936e71e8499bff5513d71b6"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
