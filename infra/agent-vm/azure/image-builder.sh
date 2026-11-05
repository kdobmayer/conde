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
# Then, with GITHUB=1 and admin on the repo (its owner), the GitHub side: the environment `agent-image` with one named
# branch policy, main, so a workflow on any other branch cannot sign in as this app and publish an image every new
# agent VM boots. A named policy holds whether or not main is protected ("protected branches only" lets every
# branch deploy while the repo has no protection rule at all); protecting main is the owner's separate step.
# And the environment's three variables. Every GitHub call is checked; at the end the script asserts the
# environment mode and that main is its only branch policy.
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

ENV="repos/$REPO/environments/agent-image"
MODE='{"custom_branch_policies":true,"protected_branches":false}'
MAIN='[{"name":"main","type":"branch"}]'
policies() { gh api --paginate "$ENV/deployment-branch-policies" --jq '.branch_policies[] | {name, type}' | jq -sc .; }
if [ "${GITHUB:-0}" != 1 ]; then
  echo "GitHub side: run this script again with GITHUB=1 as a repo admin of $REPO."; exit 0
fi
gh api -X PUT "$ENV" --input - >/dev/null <<<'{"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}'
have=$(policies)
case "$have" in
  '[]') gh api -X POST "$ENV/deployment-branch-policies" -f name=main -f type=branch >/dev/null ;;
  "$MAIN") ;;
  *) echo "the environment has other branch policies, not changed: $have" >&2; exit 1 ;;
esac
gh variable set AZURE_CLIENT_ID --repo "$REPO" --env agent-image --body "$appId"
gh variable set AZURE_TENANT_ID --repo "$REPO" --env agent-image --body "$TENANT"
gh variable set AZURE_SUBSCRIPTION_ID --repo "$REPO" --env agent-image --body "$SUB"
mode=$(gh api "$ENV" --jq '.deployment_branch_policy' | jq -Sc .)
[ "$mode" = "$MODE" ] || { echo "environment mode is $mode, expected $MODE" >&2; exit 1; }
[ "$(policies)" = "$MAIN" ] || { echo "branch policies are $(policies), expected $MAIN" >&2; exit 1; }
echo "environment agent-image: main only, variables set"
