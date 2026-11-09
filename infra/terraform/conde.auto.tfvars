image        = "crnative.azurecr.io/conde@sha256:f62352ce3b5782cd64c92f2d44c8a91cf92efe5201e89517ca4c5c78631a1e95"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
connect_audience = "9e73779d-480b-449a-a24b-6a581e370ac2" # praxia-agent-connect: the audience of the agent VMs' connect proofs to Praxia
