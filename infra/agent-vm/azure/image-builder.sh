#!/usr/bin/env bash
# One-time setup of the image workflow's Azure identity, by hand, by someone who may create Entra apps and assign
# roles in the subscription. Idempotent. Afterwards: deploy gallery.bicep with the printed builderPrincipalId.
#
#   infra/agent-vm/azure/image-builder.sh
#
# Creates: the Entra app conde-agent-image with a federated credential for this repo's `agent-image` environment
# (the subject format of conde-github-release, which uses immutable owner and repo ids), its service principal,
# the build resource group rg-agent-image (Packer's temporary VM lives there) with Contributor for the app, and
# the GitHub environment's three variables.
set -euo pipefail
SUB=60df47fc-ce8f-4f52-a970-43a412d78007
TENANT=2e51b484-b008-4fc0-8f70-555c1317b555
APP=conde-agent-image
RG=rg-agent-image
LOCATION=westeurope
REPO=DakaiGroup/conde
SUBJECT='repo:DakaiGroup@65165889/conde@1411036719:environment:agent-image'

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

gh api -X PUT "repos/$REPO/environments/agent-image" >/dev/null
gh variable set AZURE_CLIENT_ID --repo "$REPO" --env agent-image --body "$appId"
gh variable set AZURE_TENANT_ID --repo "$REPO" --env agent-image --body "$TENANT"
gh variable set AZURE_SUBSCRIPTION_ID --repo "$REPO" --env agent-image --body "$SUB"

echo "app $APP: appId $appId, builderPrincipalId $spId"
