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
  for_each = toset(["conde.cloud", "*.conde.cloud"])

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
