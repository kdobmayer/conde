image        = "crnative.azurecr.io/conde@sha256:c04e3539f084ad8abc5dc884e1192a6807ab033117a3bbe87c57addd22d764fa"
target_port  = 8080
replicas     = 2    # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
dump_enabled = true # until the dakai move to GCP is done (docs/CUTOVER-dakai.md); false removes the dump job and share
