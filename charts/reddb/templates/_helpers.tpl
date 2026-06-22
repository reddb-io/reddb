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

{{- define "reddb.cluster.fullname" -}}
{{- printf "%s-cluster" (include "reddb.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.cluster.headless" -}}
{{- printf "%s-headless" (include "reddb.cluster.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "reddb.primary.selectorLabels" -}}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/component: primary
{{- end -}}

{{- define "reddb.replica.selectorLabels" -}}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/component: replica
{{- end -}}

{{- define "reddb.cluster.selectorLabels" -}}
{{ include "reddb.selectorLabels" . }}
app.kubernetes.io/component: cluster
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

{{- define "reddb.configFileEnabled" -}}
{{- if .Values.config.file.enabled -}}
true
{{- end -}}
{{- end -}}

{{- define "reddb.configMapName" -}}
{{- if .Values.config.file.existingConfigMap -}}
{{- .Values.config.file.existingConfigMap -}}
{{- else -}}
{{- printf "%s-config" (include "reddb.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
Validate the deployment mode.
*/}}
{{- define "reddb.validateMode" -}}
{{- if not (has .Values.mode (list "standalone" "serverless" "primary-replica" "cluster")) -}}
{{- fail (printf "values.mode must be 'standalone', 'serverless', 'primary-replica', or 'cluster' (got %q)" .Values.mode) -}}
{{- end -}}
{{- if and .Values.config.file.enabled (not .Values.config.file.existingConfigMap) (empty .Values.config.file.inline) -}}
{{- fail "config.file.enabled=true requires config.file.existingConfigMap or non-empty config.file.inline" -}}
{{- end -}}
{{- if and (eq .Values.mode "cluster") .Values.auth.enabled -}}
{{- fail "auth.enabled chart-managed bootstrap is not supported in mode=cluster: a symmetric member cannot prove it is the reserved global system range owner (ADR 0058), so the runtime fails closed on a cluster-shaped credentialled boot. This gate keeps the chart fail-closed in lockstep with the runtime until the reserved-range owner path lands (PRD #1227). Run a no-auth/dev cluster, or bootstrap auth on a non-cluster topology. See charts/reddb/README.md (Cluster) and docs/deployment/first-boot.md (Cluster Bootstrap Authority)." -}}
{{- end -}}
{{- if .Values.auth.vault.bootstrapJob.enabled -}}
{{- fail "auth.vault.bootstrapJob.enabled is disabled: the legacy hook bootstraps an emptyDir DB, not the StatefulSet PVC. Run red bootstrap against the real volume or use HTTP bootstrap after the writer starts." -}}
{{- end -}}
{{- if and .Values.grpc.tls.enabled .Values.http.tls.enabled (eq .Values.grpc.tls.bindAddr .Values.http.tls.bindAddr) -}}
{{- fail "grpc.tls.bindAddr and http.tls.bindAddr cannot use the same address when both TLS listeners are enabled" -}}
{{- end -}}
{{- $preset := default "" .Values.storage.preset -}}
{{- if and $preset (not (has $preset (list "embedded" "serverless" "primary-replica-dev" "primary-replica-small" "primary-replica-production-ha" "primary-replica-backup" "primary-replica-wal-retention" "cluster"))) -}}
{{- fail (printf "values.storage.preset is not supported (got %q)" $preset) -}}
{{- end -}}
{{- $profile := default "" .Values.storage.profile -}}
{{- if and $profile (not (has $profile (list "embedded" "serverless" "primary-replica" "cluster"))) -}}
{{- fail (printf "values.storage.profile is not supported (got %q)" $profile) -}}
{{- end -}}
{{- $packaging := default "" .Values.storage.packaging -}}
{{- if and $packaging (not (has $packaging (list "single-file" "operational-directory"))) -}}
{{- fail (printf "values.storage.packaging must be 'single-file' or 'operational-directory' (got %q)" $packaging) -}}
{{- end -}}
{{- end -}}

{{- define "reddb.storagePreset" -}}
{{- if .Values.storage.preset -}}
{{- .Values.storage.preset -}}
{{- else if eq .Values.mode "serverless" -}}
serverless
{{- else if eq .Values.mode "primary-replica" -}}
primary-replica-production-ha
{{- else if eq .Values.mode "cluster" -}}
cluster
{{- else -}}
embedded
{{- end -}}
{{- end -}}

{{- define "reddb.storageProfile" -}}
{{- if .Values.storage.profile -}}
{{- .Values.storage.profile -}}
{{- else if eq .Values.mode "serverless" -}}
serverless
{{- else if eq .Values.mode "primary-replica" -}}
primary-replica
{{- else if eq .Values.mode "cluster" -}}
cluster
{{- else -}}
embedded
{{- end -}}
{{- end -}}

{{- define "reddb.storagePackaging" -}}
{{- if .Values.storage.packaging -}}
{{- .Values.storage.packaging -}}
{{- else if eq .Values.mode "standalone" -}}
single-file
{{- else -}}
operational-directory
{{- end -}}
{{- end -}}

{{- define "reddb.storageReplicaCount" -}}
{{- if eq .Values.mode "primary-replica" -}}
{{- .Values.replica.replicaCount -}}
{{- else if eq .Values.mode "cluster" -}}
{{- .Values.cluster.replicaCount -}}
{{- else -}}
0
{{- end -}}
{{- end -}}

{{- define "reddb.storageEnv" -}}
- name: REDDB_TOPOLOGY
  value: {{ .Values.mode | quote }}
- name: REDDB_STORAGE_PRESET
  value: {{ include "reddb.storagePreset" . | quote }}
- name: REDDB_STORAGE_PROFILE
  value: {{ include "reddb.storageProfile" . | quote }}
- name: REDDB_STORAGE_PACKAGING
  value: {{ include "reddb.storagePackaging" . | quote }}
{{- $replicaCount := include "reddb.storageReplicaCount" . }}
- name: REDDB_REPLICA_COUNT
  value: {{ $replicaCount | quote }}
{{- if ne (toString .Values.storage.managedBackup) "" }}
- name: REDDB_MANAGED_BACKUP
  value: {{ .Values.storage.managedBackup | quote }}
{{- end }}
{{- if ne (toString .Values.storage.walRetention) "" }}
- name: REDDB_WAL_RETENTION
  value: {{ .Values.storage.walRetention | quote }}
{{- end }}
{{- end -}}

{{- define "reddb.remoteEnv" -}}
{{- if .Values.remote.enabled }}
{{- $backend := lower .Values.remote.backend -}}
{{- $s3Configured := or (eq $backend "s3") (eq $backend "minio") (eq $backend "r2") .Values.remote.s3.endpoint .Values.remote.s3.bucket .Values.remote.s3.keyPrefix .Values.remote.s3.accessKey .Values.remote.s3.secretKey .Values.remote.s3.existingSecret -}}
- name: RED_BACKEND
  value: {{ required "remote.backend is required when remote.enabled=true" .Values.remote.backend | quote }}
{{- with .Values.remote.key }}
- name: RED_REMOTE_KEY
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.fs.path }}
- name: RED_FS_PATH
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.http.url }}
- name: RED_HTTP_BACKEND_URL
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.http.prefix }}
- name: RED_HTTP_BACKEND_PREFIX
  value: {{ . | quote }}
{{- end }}
{{- if .Values.remote.http.conditionalWrites }}
- name: RED_HTTP_CONDITIONAL_WRITES
  value: "true"
{{- end }}
{{- if .Values.remote.http.authHeader.existingSecret }}
- name: RED_HTTP_BACKEND_AUTH_HEADER
  valueFrom:
    secretKeyRef:
      name: {{ .Values.remote.http.authHeader.existingSecret }}
      key: {{ .Values.remote.http.authHeader.existingSecretKey }}
{{- else if .Values.remote.http.authHeader.value }}
- name: RED_HTTP_BACKEND_AUTH_HEADER
  value: {{ .Values.remote.http.authHeader.value | quote }}
{{- end }}
{{- if $s3Configured }}
{{- with .Values.remote.s3.endpoint }}
- name: RED_S3_ENDPOINT
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.s3.bucket }}
- name: RED_S3_BUCKET
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.s3.region }}
- name: RED_S3_REGION
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.s3.keyPrefix }}
- name: RED_S3_KEY_PREFIX
  value: {{ . | quote }}
{{- end }}
- name: RED_S3_PATH_STYLE
  value: {{ .Values.remote.s3.pathStyle | quote }}
{{- if .Values.remote.s3.existingSecret }}
- name: RED_S3_ACCESS_KEY
  valueFrom:
    secretKeyRef:
      name: {{ .Values.remote.s3.existingSecret }}
      key: {{ .Values.remote.s3.accessKeyKey }}
- name: RED_S3_SECRET_KEY
  valueFrom:
    secretKeyRef:
      name: {{ .Values.remote.s3.existingSecret }}
      key: {{ .Values.remote.s3.secretKeyKey }}
{{- else }}
{{- with .Values.remote.s3.accessKey }}
- name: RED_S3_ACCESS_KEY
  value: {{ . | quote }}
{{- end }}
{{- with .Values.remote.s3.secretKey }}
- name: RED_S3_SECRET_KEY
  value: {{ . | quote }}
{{- end }}
{{- end }}
{{- end }}
{{- end }}
{{- end -}}

{{- define "reddb.serverlessEnv" -}}
{{- if eq .Values.mode "serverless" }}
- name: RED_AUTO_RESTORE
  value: {{ .Values.serverless.autoRestore | quote }}
- name: RED_BACKUP_ON_SHUTDOWN
  value: {{ .Values.serverless.backupOnShutdown | quote }}
- name: RED_LEASE_REQUIRED
  value: {{ .Values.serverless.lease.required | quote }}
- name: RED_LEASE_TTL_SECS
  value: {{ .Values.serverless.lease.ttlSeconds | quote }}
- name: RED_LEASE_PREFIX
  value: {{ .Values.serverless.lease.prefix | quote }}
{{- end }}
{{- end -}}

{{- define "reddb.replicationEnv" -}}
{{- if eq .Values.mode "primary-replica" }}
{{- with .Values.replication.commitPolicy }}
- name: RED_PRIMARY_COMMIT_POLICY
  value: {{ . | quote }}
{{- end }}
{{- with .Values.replication.commitAckN }}
- name: RED_PRIMARY_COMMIT_ACK_N
  value: {{ . | quote }}
{{- end }}
{{- with .Values.replication.commitDeadlineMs }}
- name: RED_PRIMARY_COMMIT_DEADLINE_MS
  value: {{ . | quote }}
{{- end }}
{{- end }}
{{- end -}}

{{- define "reddb.clusterPeers" -}}
{{- if .Values.cluster.discovery.staticPeers -}}
{{- join "," .Values.cluster.discovery.staticPeers -}}
{{- else -}}
{{- $peers := list -}}
{{- $fullname := include "reddb.cluster.fullname" . -}}
{{- $headless := include "reddb.cluster.headless" . -}}
{{- $namespace := .Release.Namespace -}}
{{- $port := .Values.cluster.service.grpcPort -}}
{{- range $i := until (int .Values.cluster.replicaCount) -}}
{{- $peers = append $peers (printf "%s-%d.%s.%s.svc.cluster.local:%v" $fullname $i $headless $namespace $port) -}}
{{- end -}}
{{- join "," $peers -}}
{{- end -}}
{{- end -}}

{{- define "reddb.clusterEnv" -}}
- name: RED_CLUSTER_HA_INTENT
  value: "declared"
- name: REDDB_CLUSTER_NODE_ID
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: REDDB_CLUSTER_NAMESPACE
  valueFrom:
    fieldRef:
      fieldPath: metadata.namespace
- name: REDDB_CLUSTER_HEADLESS_SERVICE
  value: {{ include "reddb.cluster.headless" . | quote }}
- name: REDDB_CLUSTER_PEERS
  value: {{ include "reddb.clusterPeers" . | quote }}
{{- end -}}

{{- define "reddb.activeHttpServiceName" -}}
{{- if eq .Values.mode "cluster" -}}
{{- include "reddb.cluster.fullname" . -}}
{{- else -}}
{{- include "reddb.primary.fullname" . -}}
{{- end -}}
{{- end -}}

{{- define "reddb.activeHttpServicePort" -}}
{{- if eq .Values.mode "cluster" -}}
{{- .Values.cluster.service.httpPort -}}
{{- else -}}
{{- .Values.primary.service.httpPort -}}
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
- name: REDDB_VAULT
  value: {{ .Values.config.vault | quote }}
- name: POD_NAME
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: POD_NAMESPACE
  valueFrom:
    fieldRef:
      fieldPath: metadata.namespace
{{ include "reddb.storageEnv" . }}
{{ include "reddb.remoteEnv" . }}
{{ include "reddb.serverlessEnv" . }}
{{ include "reddb.replicationEnv" . }}
{{ if include "reddb.configFileEnabled" . }}
- name: REDDB_CONFIG_FILE
  value: {{ .Values.config.file.mountPath | quote }}
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

{{- define "reddb.bootstrapEnv" -}}
{{- if .Values.auth.enabled }}
- name: REDDB_PRESET
  value: "production"
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
{{- end -}}

{{- define "reddb.extraEnvFrom" -}}
{{- with .Values.config.extraEnvFrom }}
envFrom:
{{ toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{- define "reddb.configFileVolume" -}}
{{- if include "reddb.configFileEnabled" . }}
- name: reddb-config
  configMap:
    name: {{ include "reddb.configMapName" . }}
    items:
      - key: {{ .Values.config.file.key }}
        path: config.json
{{- end }}
{{- end -}}

{{- define "reddb.configFileVolumeMount" -}}
{{- if include "reddb.configFileEnabled" . }}
- name: reddb-config
  mountPath: {{ .Values.config.file.mountPath | quote }}
  subPath: config.json
  readOnly: true
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
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:5000"]
  periodSeconds: {{ .Values.probes.startup.periodSeconds }}
  failureThreshold: {{ .Values.probes.startup.failureThreshold }}
  timeoutSeconds: {{ .Values.probes.startup.timeoutSeconds }}
{{- end }}
{{- if .Values.probes.liveness.enabled }}
livenessProbe:
  exec:
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:5000"]
  initialDelaySeconds: {{ .Values.probes.liveness.initialDelaySeconds }}
  periodSeconds: {{ .Values.probes.liveness.periodSeconds }}
  timeoutSeconds: {{ .Values.probes.liveness.timeoutSeconds }}
  failureThreshold: {{ .Values.probes.liveness.failureThreshold }}
{{- end }}
{{- if .Values.probes.readiness.enabled }}
readinessProbe:
  exec:
    command: ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:5000"]
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

{{/*
Default soft pod anti-affinity for cluster members (spread across nodes).
*/}}
{{- define "reddb.cluster.defaultAffinity" -}}
podAntiAffinity:
  preferredDuringSchedulingIgnoredDuringExecution:
    - weight: 100
      podAffinityTerm:
        topologyKey: kubernetes.io/hostname
        labelSelector:
          matchLabels:
            {{- include "reddb.cluster.selectorLabels" . | nindent 12 }}
{{- end -}}
