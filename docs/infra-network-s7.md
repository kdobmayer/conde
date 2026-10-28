# S7: the network layer of conde.cloud, revisited on the live estate (desk, 2026-10-28)

## The answer

Change one thing: cut the outbound reach. Every workspace pod and Keycloak may open port 443 to any public
address today. Replace that rule with hostname-based egress policies that name the hosts they really use. This is
the one change with a concrete threat behind it. Since 2 September 2025 the feature is part of GKE at no extra cost.

Leave the other three candidates. A cluster-wide policy guards against a namespace without a policy, and no such
namespace exists. A service mesh has no requirement: one cross-application hop exists, with one caller. The
Cloudflare edge would save about 40 USD a month. In exchange, admin access leaves the project's IAM and the rate rules
weaken on the free plan. A new in-cluster proxy and a second vendor join the request path.

## What is built today, as observed

The cluster runs Dataplane V2, so network policies are enforced by Cilium at layers 3 and 4. Nodes are private;
outbound traffic leaves through Cloud NAT, and Google APIs are reached through Private Google Access.

Five network policies exist, one per namespace: the three workspaces, Keycloak and Argo CD. Each denies everything by
default and then allows a short list. Inbound: only the load balancer's ranges, to port 8080. Keycloak also accepts
9000 for health checks and the demo workspace on 8080 for user invites. Argo CD pods may talk to each other.
Outbound, for workspaces and Keycloak: DNS, the metadata server, the Cloud SQL endpoint on 3307, and port 443 to any
public address. Keycloak may also open 465 to any public address for mail. Argo CD is tighter: GitHub's published
ranges on 22 and 443, the API server, and the load balancer's address for its own sign-in.

No FQDN policy, no Cilium cluster-wide policy and no ClusterNetworkPolicy exists; those APIs are not installed on the
cluster. The namespaces without a policy are the Google-managed ones, which Autopilot does not let us police, plus
`gateway` and `default`, which run no pods.

What the workloads talk to, read from the code and the charts. A workspace reaches Postgres through its proxy
sidecar: the private endpoint, plus the Cloud SQL API over 443 (an inference from the proxy's documentation). It
reaches its sign-in issuer: Keycloak through the public load balancer, or Microsoft's login host for dakai. It calls
Slack's API. Dakai and clearscale send their invite mail through Resend's API. Demo does not: its invites come from
Keycloak, whose admin API it calls inside the cluster. Secrets arrive through the managed CSI driver in the system
namespace, so the pods never call Secret Manager themselves (an inference from the pod listing). Keycloak reaches
Postgres the same way and Resend's SMTP host on 465. For the clearscale realm its Google provider calls three Google
hosts: the token, userinfo and key endpoints (read in Keycloak's source for the pinned version). Argo CD reaches
GitHub over SSH only (its repository address is SSH), the API server, and the infra realm through the load balancer.

The edge is one global external Application Load Balancer on one static address, built from the Gateway. The three
workspaces and Keycloak carry the public Cloud Armor policy: admin-path denies, a sign-in rate ban, default allow.
The Keycloak console and Argo CD carry Identity-Aware Proxy for the owner's account plus a per-address throttle.
Cloudflare proxies only the apex and www to Pages. The edge costs about 42.5 USD a month: 18.3 for the load balancer
and 24.3 for Cloud Armor. IAP and the certificate are free.

## The four candidates

**1. Hostname egress policies: do it.**

The threat is concrete. A workspace pod runs the server that agents talk to, holds a Slack token and, on two
workspaces, a Resend key, and has its database. It may open 443 to the whole internet, so a compromised pod could
send its data anywhere. The policy shrinks that reach to a short list per namespace. It is not a wall: rules allow
resolved addresses and ports, not hostnames on the wire. A domain that shares an address with other services admits
them too. DNS lookups stay open, and the allowed Slack and Resend hosts remain channels for data. What remains is
the reduced reach, which is still worth having.

The lists, read from the code. Every workspace: Slack's API and the Cloud SQL API, by wildcard for Google's hosts.
Dakai adds Microsoft's login host and Resend's API. Clearscale adds the Keycloak host and Resend's API. Demo adds the
Keycloak host; its in-cluster call to Keycloak's admin API stays a label rule. Keycloak: the three Google hosts, the
Cloud SQL API, and Resend's SMTP host on 465. Argo CD: GitHub on 22 and the Keycloak host on 443. DNS, the metadata
server, the database endpoint, the API server and the in-cluster rules stay in the network policies as they are.

The feature exists on Autopilot and is enabled by updating the cluster. Google then restarts the nodes itself, which
the page says can take up to a few hours. The single-replica workspaces, demo and clearscale, see a short outage each.
Rules are egress-only at layers 3 and 4, honour DNS TTLs and resolve up to 50 addresses per policy. A wildcard
matches one label only. One trap: when an FQDN policy and a network policy both apply, traffic passes if either
allows it. So the broad allows must go, and the order matters because three systems move the pieces: the cluster
update, Argo CD and the node replacement. Enable in a planned window and wait until every node enforces. Install the
narrow policies next; the broad allows overlap them and keep today's reach, nothing breaks. Then remove the broad
allows one namespace at a time. After each, check fresh connections to the listed hosts and a refused one elsewhere,
with the old policy at hand to put back. Removing the broad allows before enforcement is live would cause outages.

The feature has no price line of its own. The release note of 2 September 2025 lists FQDN Network Policy among the
features "included in the GKE offering at no additional cost".

**2. Cilium cluster-wide policies: leave.**

What they add is a default deny that no namespace can forget. Every namespace that runs our pods already carries one.
The two sources that create namespaces, the workspace chart and the cluster code, carry the policy with them. A
cluster-wide policy can select only our own workloads, so the Google-managed namespaces are not the obstacle; the
missing requirement is. Enabling it on an existing cluster restarts the data plane, which the page says causes
downtime. Google's page now points new
deployments to the Kubernetes-native ClusterNetworkPolicy instead. That one is in preview at alpha level on 1.36.0
and later; our cluster is 1.36.4. Revisit when it is generally available and a third place starts creating namespaces.

**3. Cloud Service Mesh: no requirement exists.**

Argo CD's own components talk to each other inside their namespace. Beyond that, one cross-application hop exists:
the demo workspace to Keycloak's admin API, plain HTTP on 8080 inside the VPC. Everything else goes out through the
public load balancer. The policy admits that hop by namespace label, which is not an authenticated caller; the
application's client secret is the credential. A mesh would add mTLS and L7 rules to a hop that has one caller. Google's managed mesh runs on Autopilot and costs 0.0006945 USD per client-hour.
That is about 0.50 USD a client a month, roughly 4 USD for our eight pods plus the proxies' compute. Cheap, but it
serves nothing stated. If that one hop must be encrypted, Keycloak serving HTTPS on its Service is the small change.

**4. Cloudflare Tunnel plus Access as the edge: not now.**

It would replace the load balancer, Cloud Armor, IAP, the managed certificate and the static address. That is about
42.5 USD a month gone. In their place comes a tunnel connector running as two pods in the cluster, a new component.
The tunnel, its routes and the Access policies can be kept in code, like today's edge. Cloudflare's free WAF ruleset
and DDoS defence come with it, and Access sits in front of the admin hosts. Access is free for teams under 50 users
and 7 USD a user a month beyond. It signs people in with Google through its documented provider integration, so the
Google sign-in itself stays. The connector reads its token from a file (a documented flag of the current releases),
so the secret can stay a mounted file.

What is lost. IAP binds admin access to an IAM grant in the project; that ownership moves into the Cloudflare
account. Cloud Armor's rate rules do not carry over on the free plan, which counts over 10 seconds only. The built
rule counts 100 requests a minute and bans for 10 minutes; parity needs a paid plan. The per-address admin throttles
have no equivalent in Access. The earlier decision "no second proxy" is reversed: every request to a workspace would
pass Cloudflare's network. Cloudflare's proxy read timeout is 125 seconds; the streams ping every 30 seconds, so they
would survive.

What is gained: no public origin address, DDoS and WAF at no charge, and the money. Nothing today requires a hidden
origin. The money is the only argument, and the owner should weigh it (the fork below).

## Recommended changes, and the fork

1. Hostname egress policies in the three workspace namespaces, Keycloak and Argo CD, with the lists above. They
   replace the any-public-443 rule, Keycloak's any-public-465 rule and Argo CD's hand-copied GitHub ranges, a list
   that goes stale. Money: none. Operations: one cluster update with Google-run node restarts and a new policy
   template in the workspace chart, then the staged removal of the broad allows.
2. Nothing else. Cluster-wide policies, the mesh and the Cloudflare edge stay out, for the reasons above.

The fork: keep the Google edge at about 42.5 USD a month, or move to Cloudflare Tunnel and Access to save most of it?
Recommendation: keep the Google edge. The saving is real. It buys a second vendor on the request path and a new
in-cluster component. It also brings weaker rate rules unless a paid plan is added, and admin access that no longer
rests on the project's IAM. Revisit if the monthly budget must fall or if hiding the origin becomes a requirement.

Not verified today. An explicit price line for Cloudflare Tunnel. The block duration its free plan allows: the page
says the values vary by plan without printing them. The Cloud SQL proxy's exact API hosts, taken from its
documentation rather than observed; its traffic should be watched before the broad allows go.
