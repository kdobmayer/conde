# The workspace's Kubernetes objects belong to Argo CD (charts/conde-workspace, deploy/tenants/<name>.yaml). These
# blocks drop them from the state without deleting them; delete the file once every workspace has been applied.
removed {
  from = kubernetes_namespace_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_service_account_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_service_account_v1.dbinit
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_manifest.secrets
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_manifest.dbinit_secrets
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_network_policy_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_job_v1.dbinit
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_deployment_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_pod_disruption_budget_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_service_v1.this
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_manifest.route
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_manifest.health_check
  lifecycle {
    destroy = false
  }
}

removed {
  from = kubernetes_manifest.backend
  lifecycle {
    destroy = false
  }
}
