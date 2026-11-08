image        = "crnative.azurecr.io/conde@sha256:196cae029e87190e5bebdc55b233e43bee38c212015c87d02a98ff9560d5f60c"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
connect_audience = "9e73779d-480b-449a-a24b-6a581e370ac2" # praxia-agent-connect: the audience of the agent VMs' connect proofs to Praxia
