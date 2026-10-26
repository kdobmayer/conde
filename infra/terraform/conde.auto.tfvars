image        = "crnative.azurecr.io/conde:0c078ae"
target_port  = 8080
replicas     = 2 # two replicas since 2026-10-09: a single replica loss was a nine-minute outage
slack_notify = "kdobmayer=U08QZSVHGEP"
