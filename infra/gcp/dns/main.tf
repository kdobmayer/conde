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
  content = "\"v=DMARC1; p=quarantine; rua=mailto:kdobmayer.devops@gmail.com\""
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
