# Kubernetes and Helm

RedDB uses a single container image for Kubernetes. The Helm chart selects the
topology by rendering different args, env vars, StatefulSets, and Services.

## Modes

| Mode | Pods | Command | Storage preset |
|---|---:|---|---|
| `serverless` | 1 writer | `red server` | `serverless` |
| `primary-replica` | 1 primary + N replicas | primary: `red server --role primary`; replicas: `red replica` | `primary-replica-production-ha` |
| `cluster` | N symmetric members | `red server --role standalone` today, with cluster storage/discovery env | `cluster` |

`standalone` remains available for compatibility and local demos. Embedded mode
is not a Kubernetes production topology.

## Serverless

```yaml
mode: serverless

remote:
  enabled: true
  backend: s3
  key: prod/main/data.rdb
  s3:
    endpoint: https://s3.us-east-1.amazonaws.com
    bucket: reddb-prod
    region: us-east-1
    existingSecret: reddb-s3
    accessKeyKey: access-key
    secretKeyKey: secret-key

serverless:
  autoRestore: true
  backupOnShutdown: true
  lease:
    required: true
```

`serverless.lease.required=true` should be used when more than one runtime can
attach to the same remote key. The selected backend must support CAS/conditional
writes.

## Primary-Replica

```yaml
mode: primary-replica

replica:
  replicaCount: 3

replication:
  commitPolicy: quorum
```

For primary-only operation with the same storage profile:

```yaml
mode: primary-replica
replica:
  replicaCount: 0
```

The chart renders replicas only when `replica.replicaCount > 0`.

## Cluster

```yaml
mode: cluster
cluster:
  replicaCount: 3
```

The chart gives each pod a stable StatefulSet DNS name and sets
`REDDB_CLUSTER_PEERS`. The current binary does not expose a separate
`--role cluster`; cluster pods use `red server --role standalone` with
`REDDB_STORAGE_PRESET=cluster` and `RED_CLUSTER_HA_INTENT=declared`.

## Config File

```yaml
config:
  file:
    enabled: true
    inline:
      red:
        logging:
          level: info
          format: json
```

The file mounts at `/etc/reddb/config.json`, and `REDDB_CONFIG_FILE` points to
it. RedDB seeds missing keys into `red.config` on boot. Existing rows from a
prior boot, `SET CONFIG`, or boot defaults are not overwritten.

Separate boot/topology config from runtime config:

- Boot config remains args/env: role, primary address, storage preset/profile,
  remote backend, lease settings, data path, and secrets.
- Runtime config lives in `red.config`.
- Env overrides for config-matrix keys win for the current boot and are not
  persisted.

Use `SET CONFIG` for persistent config changes after first boot.

## Secrets

Use Kubernetes Secrets for credentials:

```yaml
auth:
  enabled: true
  existingSecret: reddb-admin

remote:
  enabled: true
  backend: s3
  s3:
    existingSecret: reddb-s3
```

For secret-file workflows, mount the Secret with `extraSecretMounts` and pass the
matching `*_FILE` env var through `config.extraEnv`.
