image        = "crnative.azurecr.io/conde:0.3.0"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
moved_to     = "https://dakai.conde.cloud" # the board moved (docs/CUTOVER-dakai.md step 5)
