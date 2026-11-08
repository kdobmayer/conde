# The agent VM image: Ubuntu 24.04 with everything an always-on agent needs, generalized, into the Compute
# Gallery galNativeLanes as image definition `agent-vm` (azure/gallery.bicep). Built by the GitHub workflow
# .github/workflows/agent-image.yml; by hand after `az login`:
#
#   packer init infra/agent-vm/image
#   packer build -var subscription_id=<id> -var image_version=1.<yyyymmdd>.<n> -var allowed_ip=<your IP> infra/agent-vm/image
#
# (by hand the plugin uses the az CLI session; in the workflow, GitHub OIDC)
#
# The build VM is temporary, in the build resource group, reachable over SSH from allowed_ip only. The
# provisioner is ../bootstrap.sh in image mode plus provision.sh (Chrome, the update timers, first boot).

packer {
  required_plugins {
    azure = {
      source  = "github.com/hashicorp/azure"
      version = "= 2.3.3"
    }
  }
}

variable "subscription_id" {
  type = string
}

# GitHub OIDC, straight to the azure plugin, which asks GitHub for a fresh assertion for every new Azure token (an
# az session from azure/login cannot renew and ran out an hour into a build). Empty: a manual build after az login.
variable "client_id" {
  type    = string
  default = ""
}

variable "tenant_id" {
  type    = string
  default = ""
}

variable "oidc_request_url" {
  type    = string
  default = env("ACTIONS_ID_TOKEN_REQUEST_URL")
}

variable "oidc_request_token" {
  type      = string
  default   = env("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
  sensitive = true
}

variable "image_version" {
  type        = string
  description = "Gallery image version, Major.Minor.Patch; the workflow uses 1.<yyyymmdd>.<run number>"
}

variable "build_resource_group" {
  type    = string
  default = "rg-agent-image"
}

variable "allowed_ip" {
  type        = string
  description = "The only address that may reach the build VM over SSH (the runner's public IP). Required: without it Packer would leave SSH open to the internet"

  validation {
    condition     = can(cidrhost("${var.allowed_ip}/32", 0))
    error_message = "The allowed_ip must be one IPv4 address."
  }
}

variable "connect_resource" {
  type        = string
  default     = ""
  description = "Praxia's praxia-agent-connect application id (/etc/agentvm.conf CONNECT_RESOURCE); empty: the VMs never connect to Praxia"
}

variable "build_vm_size" {
  type    = string
  default = "Standard_D2as_v5"
}

source "azure-arm" "agent" {
  use_azure_cli_auth        = var.oidc_request_url == ""
  client_id                 = var.client_id
  tenant_id                 = var.tenant_id
  oidc_request_url          = var.oidc_request_url
  oidc_request_token        = var.oidc_request_token
  subscription_id           = var.subscription_id
  build_resource_group_name = var.build_resource_group

  os_type         = "Linux"
  image_publisher = "Canonical"
  image_offer     = "ubuntu-24_04-lts"
  image_sku       = "server"
  vm_size         = var.build_vm_size
  os_disk_size_gb = 64

  allowed_inbound_ip_addresses = [var.allowed_ip]

  shared_image_gallery_destination {
    subscription         = var.subscription_id
    resource_group       = "RG-NATIVE-LANES"
    gallery_name         = "galNativeLanes"
    image_name           = "agent-vm"
    image_version        = var.image_version
    replication_regions  = ["westeurope"]
    storage_account_type = "Standard_LRS"
  }

  azure_tags = {
    purpose = "agent-vm-image-build"
  }
}

build {
  sources = ["source.azure-arm.agent"]

  provisioner "shell" {
    inline = ["mkdir -p /tmp/agentvm"]
  }

  provisioner "file" {
    sources = [
      "${path.root}/../bootstrap.sh",
      "${path.root}/../agentvm",
      "${path.root}/../run-agent",
      "${path.root}/../conde-register-hook",
      "${path.root}/../claude-agent.service",
      "${path.root}/../agentvm-firstboot",
      "${path.root}/../agentvm-firstboot.service",
      "${path.root}/../relay-claude",
      "${path.root}/../relay-github",
      "${path.root}/../relay-codex",
      "${path.root}/../agentvm-signin-check",
      "${path.root}/../agentvm-signin-check.service",
      "${path.root}/../agentvm-signin-check.timer",
      "${path.root}/../praxia-connect",
      "${path.root}/../praxia-connect.service",
      "${path.root}/../praxia-connect.timer",
      "${path.root}/provision.sh",
    ]
    destination = "/tmp/agentvm/"
  }

  provisioner "shell" {
    execute_command  = "chmod +x {{ .Path }}; {{ .Vars }} sudo -E bash '{{ .Path }}'"
    environment_vars = ["CONNECT_RESOURCE=${var.connect_resource}"]
    inline           = ["bash /tmp/agentvm/provision.sh"]
  }

  # Generalize last: removes the build user (packer) and the machine's identity, so every VM is new.
  provisioner "shell" {
    execute_command = "chmod +x {{ .Path }}; {{ .Vars }} sudo -E sh '{{ .Path }}'"
    inline          = ["/usr/sbin/waagent -force -deprovision+user && export HISTSIZE=0 && sync"]
    inline_shebang  = "/bin/sh -x"
  }
}
