# HashiCorp Nomad reference deploy (PLAN.md Phase 10.1).
#
# Submit with:
#   nomad job run reddb.nomad.hcl
#
# Notes:
# - Single-instance writer with `count = 1` and `unique_hostname`
#   constraint keeps the writer pinned to one node. Use a separate
#   `reddb-replica` job for replicas.
# - Persistent volume via CSI plugin (e.g. AWS EBS, GCP PD, Ceph).
# - Vault integration for secrets — see `template { vault = ... }`.

job "reddb" {
  type        = "service"
  datacenters = ["dc1"]

  group "primary" {
    count = 1

    constraint {
      operator  = "distinct_hosts"
      value     = "true"
    }

    update {
      max_parallel  = 1
      health_check  = "checks"
      min_healthy_time = "30s"
      healthy_deadline = "5m"
      progress_deadline = "10m"
      auto_revert  = true
      canary       = 0
    }

    network {
      port "http"  { to = 8080 }
      port "grpc"  { to = 50051 }
    }

    volume "data" {
      type            = "csi"
      source          = "reddb-data"
      read_only       = false
      attachment_mode = "file-system"
      access_mode     = "single-node-writer"
    }

    task "engine" {
      driver = "docker"

      kill_signal  = "SIGTERM"
      kill_timeout = "60s"

      config {
        image = "reddb-benchmark/reddb:latest"
        ports = ["http", "grpc"]
      }

      env {
        REDDB_HTTP_BIND_ADDR     = "0.0.0.0:8080"
        REDDB_GRPC_BIND_ADDR     = "0.0.0.0:50051"
        RED_BACKEND              = "s3"
        RED_S3_BUCKET            = "reddb-prod"
        RED_S3_REGION            = "auto"
        RED_AUTO_RESTORE         = "true"
        RED_BACKUP_ON_SHUTDOWN   = "true"
        RED_BACKUP_INTERVAL_SECS = "300"
        RED_LOG_FORMAT           = "json"
        RED_LOG_LEVEL            = "info"
        RED_LEASE_REQUIRED       = "true"
        RED_SHUTDOWN_TIMEOUT_SECS = "60"
      }

      vault {
        policies = ["reddb-read"]
        change_mode = "restart"
      }

      # Encrypted-vault key — fetched from Vault KV and written into the
      # task's runtime secrets dir. The binary reads REDDB_CERTIFICATE_FILE
      # and the path never appears in the audited env. Bootstrap once via
      # `red bootstrap --print-certificate` and `vault kv put reddb/vault
      # certificate=$CERT`. BACK IT UP — there is no recovery.
      template {
        destination = "secrets/vault-cert"
        perms       = "0400"
        data        = <<EOH
{{ with secret "reddb/data/vault" }}{{ .Data.data.certificate }}{{ end }}
EOH
      }

      template {
        destination = "secrets/s3.env"
        env         = true
        data        = <<EOH
REDDB_CERTIFICATE_FILE=/secrets/vault-cert
{{ with secret "reddb/data/s3" }}
RED_S3_ACCESS_KEY={{ .Data.data.access_key }}
RED_S3_SECRET_KEY={{ .Data.data.secret_key }}
{{ end }}
{{ with secret "reddb/data/admin" }}
RED_ADMIN_TOKEN={{ .Data.data.token }}
{{ end }}
EOH
      }

      volume_mount {
        volume      = "data"
        destination = "/data"
        read_only   = false
      }

      resources {
        cpu    = 1000
        memory = 2048
      }

      service {
        name = "reddb-http"
        port = "http"
        check {
          type     = "http"
          path     = "/health/live"
          interval = "10s"
          timeout  = "2s"
        }
        check {
          type     = "http"
          path     = "/health/ready"
          interval = "10s"
          timeout  = "2s"
        }
      }

      service {
        name = "reddb-grpc"
        port = "grpc"
        check {
          type     = "tcp"
          interval = "15s"
          timeout  = "2s"
        }
      }
    }
  }
}
