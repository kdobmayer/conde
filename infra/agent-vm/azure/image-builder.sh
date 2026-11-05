#!/usr/bin/env bash
# One-time setup of the image workflow's Azure identity, by hand, by someone who may create Entra apps and assign
# roles in the subscription. Idempotent. Afterwards: deploy gallery.bicep with the printed builderPrincipalId.
#
#   infra/agent-vm/azure/image-builder.sh
#
# Creates: the Entra app conde-agent-image with a federated credential for the repo's `agent-image` environment
# (the repo's OIDC subject uses immutable owner and repo ids: kdobmayer/conde's prefix is
# repo:kdobmayer@22565526/conde@1436388412), its service principal, and the build resource group rg-agent-image
# (Packer's temporary VM lives there) with Contributor for the app.
#
# Then the GitHub side, which needs admin on the repo (its owner): the environment `agent-image` with one named
# branch policy, main, so a workflow on any other branch cannot sign in as this app and publish an image every new
# agent VM boots. A named policy holds whether or not main is protected ("protected branches only" lets every
# branch deploy while the repo has no protection rule at all); protecting main is the owner's separate step.
# And the environment's three variables. Run with GITHUB=1 as the repo admin, or have them
# run the four gh commands printed at the end.
set -euo pipefail
SUB=60df47fc-ce8f-4f52-a970-43a412d78007
TENANT=2e51b484-b008-4fc0-8f70-555c1317b555
APP=conde-agent-image
RG=rg-agent-image
LOCATION=westeurope
REPO=kdobmayer/conde
SUBJECT='repo:kdobmayer@22565526/conde@1436388412:environment:agent-image'

appId=$(az ad app list --display-name "$APP" --query '[0].appId' -o tsv)
[ -n "$appId" ] || appId=$(az ad app create --display-name "$APP" --sign-in-audience AzureADMyOrg --query appId -o tsv)
spId=$(az ad sp show --id "$appId" --query id -o tsv 2>/dev/null || az ad sp create --id "$appId" --query id -o tsv)

if ! az ad app federated-credential list --id "$appId" --query "[?subject=='$SUBJECT']" -o tsv | grep -q .; then
  az ad app federated-credential create --id "$appId" --parameters "$(jq -n --arg s "$SUBJECT" \
    '{name: "github-agent-image", issuer: "https://token.actions.githubusercontent.com", subject: $s, audiences: ["api://AzureADTokenExchange"]}')" >/dev/null
fi

az group create --subscription "$SUB" -n "$RG" -l "$LOCATION" --tags purpose=agent-vm-image-build >/dev/null
az role assignment create --assignee-object-id "$spId" --assignee-principal-type ServicePrincipal \
  --role Contributor --scope "/subscriptions/$SUB/resourceGroups/$RG" >/dev/null

echo "app $APP: appId $appId, builderPrincipalId $spId"

gh_cmds=(
  "gh api -X PUT repos/$REPO/environments/agent-image --input - <<<'{\"deployment_branch_policy\":{\"protected_branches\":false,\"custom_branch_policies\":true}}'"
  "gh api -X POST repos/$REPO/environments/agent-image/deployment-branch-policies -f name=main -f type=branch"
  "gh variable set AZURE_CLIENT_ID --repo $REPO --env agent-image --body $appId"
  "gh variable set AZURE_TENANT_ID --repo $REPO --env agent-image --body $TENANT"
  "gh variable set AZURE_SUBSCRIPTION_ID --repo $REPO --env agent-image --body $SUB"
)
if [ "${GITHUB:-0}" = 1 ]; then
  for c in "${gh_cmds[@]}"; do eval "$c" >/dev/null || true; done   # the branch rule exists already on a re-run
  gh api "repos/$REPO/environments/agent-image/deployment-branch-policies" --jq '[.branch_policies[].name]'
else
  echo "GitHub side, for the repo admin of $REPO:"; printf '  %s\n' "${gh_cmds[@]}"
fi
