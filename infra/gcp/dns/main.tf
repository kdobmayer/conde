# conde.cloud at Cloudflare: DNS only (grey cloud), every record a Terraform resource. The certificate
# authorization and the gateway address come from the platform root.
data "terraform_remote_state" "platform" {
  backend = "gcs"
  config = {
    bucket                      = "conde-tfstate-476201321288"
    prefix                      = "platform"
    impersonate_service_account = "terraform@conde-prod.iam.gserviceaccount.com"
  }
}

data "cloudflare_zone" "conde" {
  filter = {
    name = "conde.cloud"
  }
}

locals {
  zone_id = data.cloudflare_zone.conde.zone_id
  auth    = data.terraform_remote_state.platform.outputs.dns_authorization_record
  gateway = data.terraform_remote_state.platform.outputs.gateway_ip_address
}

# Certificate Manager's DNS authorization: the name already carries the trailing dot.
resource "cloudflare_dns_record" "certificate_authorization" {
  zone_id = local.zone_id
  name    = trimsuffix(local.auth.name, ".")
  type    = local.auth.type
  content = trimsuffix(local.auth.data, ".")
  ttl     = 300
  proxied = false
}

resource "cloudflare_dns_record" "gateway" {
  for_each = toset(["*.conde.cloud"])

  zone_id = local.zone_id
  name    = each.value
  type    = "A"
  content = local.gateway
  ttl     = 300
  proxied = false
}

output "records" {
  value = { for k, r in merge({ auth = cloudflare_dns_record.certificate_authorization }, cloudflare_dns_record.gateway) : k => "${r.name} ${r.type} ${r.content}" }
}

# Outbound mail (Resend, EU region): DKIM, the bounce MX and SPF on the `send` subdomain, click tracking,
# and a DMARC policy so nothing else can send as conde.cloud.
resource "cloudflare_dns_record" "resend_dkim" {
  zone_id = local.zone_id
  name    = "resend._domainkey.conde.cloud"
  type    = "TXT"
  content = "\"p=MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCsOxePB89Eghh7LbwzRVXfeCDu5tULAjO8+3Awd7y2pOJjtooQJF60GSZGdAVzKDhiC3+DQAtkUip4tfxi2osbCiUroWcxIcu2zQy2RUCw1SFI781s0ti8E/v+C61yePhXVC3thFm4KIs0F4yfQOPlevD6nDVhvkIpRrLYhQk4YwIDAQAB\""
  ttl     = 300
}

resource "cloudflare_dns_record" "resend_bounce_mx" {
  zone_id  = local.zone_id
  name     = "send.conde.cloud"
  type     = "MX"
  content  = "feedback-smtp.eu-west-1.amazonses.com"
  priority = 10
  ttl      = 300
}

resource "cloudflare_dns_record" "resend_spf" {
  zone_id = local.zone_id
  name    = "send.conde.cloud"
  type    = "TXT"
  content = "\"v=spf1 include:amazonses.com ~all\""
  ttl     = 300
}

resource "cloudflare_dns_record" "resend_tracking" {
  zone_id = local.zone_id
  name    = "rsend.conde.cloud"
  type    = "CNAME"
  content = "send.forge.rmta.net"
  ttl     = 300
  proxied = false
}

resource "cloudflare_dns_record" "dmarc" {
  zone_id = local.zone_id
  name    = "_dmarc.conde.cloud"
  type    = "TXT"
  content = "\"v=DMARC1; p=quarantine; rua=mailto:krisztian@conde.cloud\""
  ttl     = 300
}

# The public site (landing, docs, install.sh) on Cloudflare Pages: the apex and www are proxied CNAMEs to the
# project; the wildcard above keeps every workspace hostname on the Google load balancer, DNS only.
resource "cloudflare_pages_project" "site" {
  account_id        = "0688c903526660bfe715c18ceb88d8e3"
  name              = "conde-cloud"
  production_branch = "main"
}

resource "cloudflare_pages_domain" "site" {
  for_each     = toset(["conde.cloud", "www.conde.cloud"])
  account_id   = cloudflare_pages_project.site.account_id
  project_name = cloudflare_pages_project.site.name
  name         = each.value
}

resource "cloudflare_dns_record" "site" {
  for_each = toset(["conde.cloud", "www.conde.cloud"])
  zone_id  = local.zone_id
  name     = each.value
  type     = "CNAME"
  content  = "${cloudflare_pages_project.site.name}.pages.dev"
  ttl      = 1
  proxied  = true
}

# Google Workspace mail for conde.cloud (the owner's mailbox krisztian@conde.cloud): the domain verification, the
# MX records and the apex SPF were written by Google through Cloudflare on 2026-10-28 and imported; DKIM is published
# here. Resend keeps its own SPF and DKIM on the send subdomain, so the two senders do not meet.
locals {
  google_mx = {
    "aspmx.l.google.com"      = 1
    "alt1.aspmx.l.google.com" = 5
    "alt2.aspmx.l.google.com" = 5
    "alt3.aspmx.l.google.com" = 10
    "alt4.aspmx.l.google.com" = 10
  }
}

resource "cloudflare_dns_record" "google_mx" {
  for_each = local.google_mx

  zone_id  = local.zone_id
  name     = "conde.cloud"
  type     = "MX"
  content  = each.key
  priority = each.value
  ttl      = 3600
}

resource "cloudflare_dns_record" "google_spf" {
  zone_id = local.zone_id
  name    = "conde.cloud"
  type    = "TXT"
  content = "\"v=spf1 include:_spf.google.com ~all\""
  ttl     = 3600
}

resource "cloudflare_dns_record" "google_verification" {
  zone_id = local.zone_id
  name    = "conde.cloud"
  type    = "TXT"
  content = "\"google-site-verification=KS1hE9-mOY8T2vmG-m0_hK3bIWety8eKzR8nDorz1Bo\""
  ttl     = 3600
}

resource "cloudflare_dns_record" "google_dkim" {
  zone_id = local.zone_id
  name    = "google._domainkey.conde.cloud"
  type    = "TXT"
  content = "\"v=DKIM1;k=rsa;p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAyoxyD1SCWzHCfGYznl7jLXU59a7l+tKRmZ4QAs+YFtIhqkBcic5g0vIGJzoooyE1gGiA5lxVfDbNy6Uv3N5+E9Vyr7hY53d4XyInfJFC8Rm/dm3yI89qFjmbYNnwNj3sss68ivX2HjtGWvJc4ZnoqbZzB8dIIA8czRLmzXAYkeMRuo9Dh7ZvztdC5SlfepZqyOew5lq9XB+3C5jtcrhG1dcdfOOFlAGx9uKpa841VmgUVDTltkWY/o4Cl8DyP8uPJd1VGK3MPvpcXNdJibGSHvlF7conveLmgAsQ8OWSgXUjePRka5wlJgFngh8FRlr0JcPc6eZcHbRu3atyvYjFSQIDAQAB\""
  ttl     = 3600
}

import {
  to = cloudflare_dns_record.google_mx["aspmx.l.google.com"]
  id = "${local.zone_id}/f0b6c6436e60c1c943dc383e596ffe6f"
}
import {
  to = cloudflare_dns_record.google_mx["alt1.aspmx.l.google.com"]
  id = "${local.zone_id}/9ee31754029db090603b9b82eb80c04b"
}
import {
  to = cloudflare_dns_record.google_mx["alt2.aspmx.l.google.com"]
  id = "${local.zone_id}/770ae4657d912deb645ca15b08e92b99"
}
import {
  to = cloudflare_dns_record.google_mx["alt3.aspmx.l.google.com"]
  id = "${local.zone_id}/4d17f0d0deb541882bcf5ead85a4a76c"
}
import {
  to = cloudflare_dns_record.google_mx["alt4.aspmx.l.google.com"]
  id = "${local.zone_id}/57bf1e4ace2485e86341f00a71210b98"
}
import {
  to = cloudflare_dns_record.google_spf
  id = "${local.zone_id}/27867b5d0bc1a11e831628494147fe29"
}
import {
  to = cloudflare_dns_record.google_verification
  id = "${local.zone_id}/5c45b868ba5076baa2127c5871828cbf"
}
