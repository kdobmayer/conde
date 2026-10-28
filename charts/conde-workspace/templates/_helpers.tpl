{{- define "conde.name" -}}
{{- required "name is required" .Values.name -}}
{{- end -}}

{{/* Selector and object labels; they are the Deployment selector, so they never change for a release. */}}
{{- define "conde.labels" -}}
app.kubernetes.io/name: conde
conde.cloud/workspace: {{ include "conde.name" . }}
{{- end -}}

{{/* A digest pins the image by itself (repository@digest); the tag then only names the build. */}}
{{- define "conde.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository .Values.image.tag -}}
{{- end -}}
{{- end -}}

{{- define "conde.database" -}}
{{- default (printf "conde_%s" (include "conde.name" .)) .Values.gcp.database -}}
{{- end -}}

{{/* Cloud SQL names an IAM service-account user by its email without the .gserviceaccount.com suffix. */}}
{{- define "conde.dbUser" -}}
{{- trimSuffix ".gserviceaccount.com" (required "gcp.serviceAccount is required" .Values.gcp.serviceAccount) -}}
{{- end -}}

{{- define "conde.envSecretName" -}}
{{- if .Values.externalSecret.enabled -}}{{ printf "%s-env" .Release.Name }}{{- else -}}{{ .Values.existingSecret }}{{- end -}}
{{- end -}}

{{- define "conde.proxyArgs" -}}
- --psc
- --structured-logs
- --address=127.0.0.1
- --port=5432
- --health-check
- --http-address=0.0.0.0
- --http-port=9090
{{- end -}}

{{/* Native sidecar: the main container starts only after the proxy's listener is up. */}}
{{- define "conde.proxySidecar" -}}
- name: cloud-sql-proxy
  image: {{ .ctx.Values.gcp.cloudSqlProxyImage }}
  restartPolicy: Always
  args:
    {{- include "conde.proxyArgs" .ctx | nindent 4 }}
    {{- if .iam }}
    - --auto-iam-authn
    {{- end }}
    - {{ required "gcp.sqlConnectionName is required" .ctx.Values.gcp.sqlConnectionName }}
  startupProbe:
    httpGet:
      path: /startup
      port: 9090
    periodSeconds: 2
    failureThreshold: 60
  # 50m is Autopilot's per-Pod CPU minimum; the proxy idles near 10Mi; the kubelet counts its logs as ephemeral storage.
  resources:
    requests:
      cpu: 50m
      memory: 64Mi
      ephemeral-storage: 100Mi
    limits:
      ephemeral-storage: 100Mi
  securityContext:
    allowPrivilegeEscalation: false
    readOnlyRootFilesystem: true
    capabilities:
      drop:
        - ALL
{{- end -}}

{{- define "conde.podSecurityContext" -}}
securityContext:
  runAsNonRoot: true
  seccompProfile:
    type: RuntimeDefault
{{- end -}}
