---
title: Self-hosting
description: Run a conde workspace on your own Kubernetes cluster with the conde-workspace Helm chart.
---

A workspace is one conde server with one PostgreSQL database and one OIDC issuer. The `conde-workspace` Helm chart installs it: the Deployment, its Service, a Gateway API route, a PodDisruptionBudget and a deny-by-default NetworkPolicy. One release per workspace, each in its own namespace; the object names are fixed, so two workspaces never share a namespace.

The chart lives in the conde repository at `charts/conde-workspace`. Its README is the reference for every value; this page is the shape.

## What you need

- A Kubernetes cluster with Helm 3 and a Gateway API gateway that terminates TLS for the workspace's hostname.
- A PostgreSQL database that the workspace's database user owns. conde runs its own schema migrations at server start, under an advisory lock, so no separate migration step or role is needed.
- An OIDC issuer the humans sign in against (your identity provider, or a Keycloak realm in front of it), with a client whose redirect URI is `https://<hostname>/auth/callback`.
- Optionally a Slack bot token with `chat:write`, for agents to page their humans.

## Install

```sh
helm install conde charts/conde-workspace -n ws-acme --create-namespace -f acme.yaml
```

The minimum `acme.yaml`:

```yaml
name: acme
hostname: conde.acme.example
oidc:
  issuer: https://login.acme.example/realms/acme
  clientId: conde
existingSecret: conde-env
```

| Value | Meaning |
|---|---|
| `name` | The workspace name: the default board name and the database name. |
| `hostname` | The public hostname; the OIDC redirect is `https://<hostname>/auth/callback`. |
| `oidc.issuer`, `oidc.clientId` | The issuer the server signs in against and its client id. |
| `replicas` | Server replicas, default 2. |
| `route.gateway.name`, `route.gateway.namespace`, `route.gateway.sectionName` | The Gateway and listener the HTTPRoute attaches to. |
| `networkPolicy.ingressCidrs`, `networkPolicy.egress` | Who may reach port 8080, and what the server may reach besides DNS (your identity provider, Slack). |

## Secrets

Outside GKE, the container's environment comes from one Kubernetes Secret. Either you create it and name it in `existingSecret`, or External Secrets Operator creates it from your store (`externalSecret.enabled`, `externalSecret.secretStoreRef`, `externalSecret.data`). The Secret holds:

| Key | Value |
|---|---|
| `DATABASE_URL` | `postgresql://user:password@host:5432/db` of a database the user owns. |
| `CONDE_ADMIN_KEY` | The admin API key, 32 or more random characters. |
| `OIDC_CLIENT_SECRET` | The client secret of the server's OIDC client. |
| `CONDE_SLACK_BOT_TOKEN` | Optional. The Slack bot token. |

On GKE (`gcp.enabled: true`), no Kubernetes Secret carries a value: the server reaches Cloud SQL through the Cloud SQL Auth Proxy sidecar with IAM authentication, so the database URL holds no password, and the admin key, the OIDC client secret and the Slack token are Secret Manager secrets mounted as files under `/secrets` by the GKE Secret Manager CSI add-on. The `gcp.*` values name the project, the service account, the Cloud SQL connection and the secret ids.

## Then

Point the workspace's DNS name at the gateway, and have each machine sign in:

```sh
conde login --server https://conde.acme.example
```

The chart's README covers the remaining values: image pinning by digest, resources, tolerations, the backend timeout for the long-lived streams, and the one-time database hand-over Job on GKE.
