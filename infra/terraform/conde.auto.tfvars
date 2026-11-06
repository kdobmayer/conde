image        = "crnative.azurecr.io/conde@sha256:5426faeaad527867448d1b69ce7834e8aa0f120f1e66161ac7121b063945e354"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
