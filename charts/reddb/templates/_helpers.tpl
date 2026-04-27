{{/*
Expand the name of the chart.
*/}}
{{- define "reddb.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully qualified app name.
*/}}
{{- define "reddb.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "reddb.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels.
*/}}
{{- define "reddb.labels" -}}
helm.sh/chart: {{ include "reddb.chart" . }}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: reddb
{{- end -}}

{{- define "reddb.selectorLabels" -}}
app.kubernetes.io/name: {{ include "reddb.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Component-specific names.
*/}}
{{- define "reddb.primary.fullname" -}}
{{- printf "%s-primary" (include "reddb.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.primary.headless" -}}
{{- printf "%s-headless" (include "reddb.primary.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.replica.fullname" -}}
{{- printf "%s-replica" (include "reddb.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.replica.headless" -}}
{{- printf "%s-headless" (include "reddb.replica.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.primary.selectorLabels" -}}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/component: primary
{{- end -}}

{{- define "reddb.replica.selectorLabels" -}}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/component: replica
{{- end -}}

{{/*
ServiceAccount name.
*/}}
{{- define "reddb.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "reddb.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
Image reference.
*/}}
{{- define "reddb.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}

{{/*
Auth secret name.
*/}}
{{- define "reddb.authSecretName" -}}
{{- if .Values.auth.existingSecret -}}
{{- .Values.auth.existingSecret -}}
{{- else -}}
{{- printf "%s-auth" (include "reddb.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Vault certificate secret name. When existingSecret is set we use that.
Otherwise we route through the chart-managed secret (created when value is set).
*/}}
{{- define "reddb.vaultCertSecretName" -}}
{{- if .Values.auth.vault.certificate.existingSecret -}}
{{- .Values.auth.vault.certificate.existingSecret -}}
{{- else -}}
{{- include "reddb.authSecretName" . -}}
{{- end -}}
{{- end -}}

{{- define "reddb.vaultCertSecretKey" -}}
{{- default "certificate" .Values.auth.vault.certificate.existingSecretKey -}}
{{- end -}}

{{/*
Whether the chart should emit the REDDB_CERTIFICATE / REDDB_CERTIFICATE_FILE
plumbing at all. Returns "true" or empty.
*/}}
{{- define "reddb.vaultCertConfigured" -}}
{{- if and .Values.auth.vault.enabled (or .Values.auth.vault.certificate.value .Values.auth.vault.certificate.existingSecret) -}}
true
{{- end -}}
{{- end -}}

{{/*
Validate the deployment mode.
*/}}
{{- define "reddb.validateMode" -}}
{{- if not (has .Values.mode (list "standalone" "primary-replica")) -}}
{{- fail (printf "values.mode must be 'standalone' or 'primary-replica' (got %q)" .Values.mode) -}}
{{- end -}}
{{- end -}}

{{/*
Common environment variables for both primary and replica pods.
*/}}
{{- define "reddb.commonEnv" -}}
- name: RUST_LOG
  value: {{ .Values.config.logLevel | quote }}
- name: REDDB_DATA_PATH
  value: /data/data.rdb
- name: POD_NAME
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: POD_NAMESPACE
  valueFrom:
    fieldRef:
      fieldPath: metadata.namespace
{{- if .Values.auth.enabled }}
- name: REDDB_USERNAME
  valueFrom:
    secretKeyRef:
      name: {{ include "reddb.authSecretName" . }}
      key: username
- name: REDDB_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "reddb.authSecretName" . }}
      key: password
{{- end }}
{{- if include "reddb.vaultCertConfigured" . }}
{{- if .Values.auth.vault.certificate.fileMount.enabled }}
- name: REDDB_CERTIFICATE_FILE
  value: {{ .Values.auth.vault.certificate.fileMount.path | quote }}
{{- else }}
- name: REDDB_CERTIFICATE
  valueFrom:
    secretKeyRef:
      name: {{ include "reddb.vaultCertSecretName" . }}
      key: {{ include "reddb.vaultCertSecretKey" . }}
{{- end }}
{{- end }}
{{- with .Values.config.extraEnv }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/*
Vault certificate volume + volumeMount snippets. Only emitted when
fileMount is enabled. Pair the two helpers in the StatefulSet template.
*/}}
{{- define "reddb.vaultVolume" -}}
{{- if and (include "reddb.vaultCertConfigured" .) .Values.auth.vault.certificate.fileMount.enabled }}
- name: reddb-vault-cert
  secret:
    secretName: {{ include "reddb.vaultCertSecretName" . }}
    defaultMode: 0400
{{- end }}
{{- end -}}

{{- define "reddb.vaultVolumeMount" -}}
{{- if and (include "reddb.vaultCertConfigured" .) .Values.auth.vault.certificate.fileMount.enabled }}
- name: reddb-vault-cert
  mountPath: {{ .Values.auth.vault.certificate.fileMount.path | quote }}
  subPath: {{ include "reddb.vaultCertSecretKey" . | quote }}
  readOnly: true
{{- end }}
{{- end -}}

{{/*
Extra arbitrary secret mounts (operator-supplied list).
*/}}
{{- define "reddb.extraSecretVolumes" -}}
{{- range .Values.extraSecretMounts }}
- name: {{ .name }}
  secret:
    secretName: {{ .secretName }}
    defaultMode: 0400
    {{- with .items }}
    items:
      {{- toYaml . | nindent 6 }}
    {{- end }}
{{- end }}
{{- end -}}

{{- define "reddb.extraSecretVolumeMounts" -}}
{{- range .Values.extraSecretMounts }}
- name: {{ .name }}
  mountPath: {{ .mountPath | quote }}
  readOnly: {{ default true .readOnly }}
{{- end }}
{{- end -}}

{{/*
Probes block.
Usage: {{ include "reddb.probes" . | nindent 10 }}
*/}}
{{- define "reddb.probes" -}}
{{- if .Values.probes.startup.enabled }}
startupProbe:
  exec:
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:8080"]
  periodSeconds: {{ .Values.probes.startup.periodSeconds }}
  failureThreshold: {{ .Values.probes.startup.failureThreshold }}
  timeoutSeconds: {{ .Values.probes.startup.timeoutSeconds }}
{{- end }}
{{- if .Values.probes.liveness.enabled }}
livenessProbe:
  exec:
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:8080"]
  initialDelaySeconds: {{ .Values.probes.liveness.initialDelaySeconds }}
  periodSeconds: {{ .Values.probes.liveness.periodSeconds }}
  timeoutSeconds: {{ .Values.probes.liveness.timeoutSeconds }}
  failureThreshold: {{ .Values.probes.liveness.failureThreshold }}
{{- end }}
{{- if .Values.probes.readiness.enabled }}
readinessProbe:
  exec:
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:8080"]
  initialDelaySeconds: {{ .Values.probes.readiness.initialDelaySeconds }}
  periodSeconds: {{ .Values.probes.readiness.periodSeconds }}
  timeoutSeconds: {{ .Values.probes.readiness.timeoutSeconds }}
  failureThreshold: {{ .Values.probes.readiness.failureThreshold }}
{{- end }}
{{- end -}}

{{/*
Default soft pod anti-affinity for replicas (spread across nodes).
*/}}
{{- define "reddb.replica.defaultAffinity" -}}
podAntiAffinity:
  preferredDuringSchedulingIgnoredDuringExecution:
    - weight: 100
      podAffinityTerm:
        topologyKey: kubernetes.io/hostname
        labelSelector:
          matchLabels:
            {{- include "reddb.replica.selectorLabels" . | nindent 12 }}
{{- end -}}
