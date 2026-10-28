#!/usr/bin/env bash
# Runs terraform in this root against Keycloak through a port-forward to its Service, over the GKE DNS endpoint as
# terraform@conde-prod: ./run.sh plan, ./run.sh apply. The Keycloak credentials come from the environment (README.md).
set -euo pipefail

sa=${GOOGLE_IMPERSONATE_SERVICE_ACCOUNT:-terraform@conde-prod.iam.gserviceaccount.com}
port=${KEYCLOAK_FORWARD_PORT:-18080}
cd "$(dirname "$0")"

# A listener already on the port would answer for Keycloak and receive the credentials.
if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
  echo "run.sh: 127.0.0.1:$port is already in use; set KEYCLOAK_FORWARD_PORT to a free port" >&2
  exit 1
fi

endpoint=$(gcloud container clusters describe conde --location europe-west3 --project conde-prod \
  --impersonate-service-account "$sa" --format 'value(controlPlaneEndpointsConfig.dnsEndpointConfig.endpoint)' 2>/dev/null)
[[ -n $endpoint ]] || { echo "run.sh: cannot read the cluster's DNS endpoint" >&2; exit 1; }

# The token goes to kubectl as a kubeconfig on a pipe: not on a command line, not in a file.
kubeconfig() {
  local token
  token=$(gcloud auth print-access-token --impersonate-service-account "$sa" 2>/dev/null)
  printf 'apiVersion: v1\nkind: Config\nclusters: [{name: conde, cluster: {server: "https://%s"}}]\nusers: [{name: terraform, user: {token: "%s"}}]\ncontexts: [{name: conde, context: {cluster: conde, user: terraform}}]\ncurrent-context: conde\n' \
    "$endpoint" "$token"
}

kubectl --kubeconfig <(kubeconfig) port-forward -n keycloak svc/keycloak "$port:8080" --address=127.0.0.1 >/dev/null &
forward=$!
trap 'kill "$forward" 2>/dev/null || true; wait "$forward" 2>/dev/null || true' EXIT

for _ in $(seq 60); do
  kill -0 "$forward" 2>/dev/null || { echo "run.sh: the port-forward exited" >&2; exit 1; }
  curl -fsS -o /dev/null "http://127.0.0.1:$port/realms/master" 2>/dev/null && break
  sleep 0.5
done
if ! { kill -0 "$forward" 2>/dev/null && curl -fsS -o /dev/null "http://127.0.0.1:$port/realms/master"; }; then
  echo "run.sh: Keycloak is not answering through the port-forward on :$port" >&2
  exit 1
fi

status=0
TF_VAR_provider_url="http://127.0.0.1:$port" "${TERRAFORM:-terraform}" "$@" || status=$?
kill -0 "$forward" 2>/dev/null || { echo "run.sh: the port-forward died during the run" >&2; exit 1; }
exit "$status"
