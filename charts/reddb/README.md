# RedDB Helm Chart

Production-oriented Helm chart for [RedDB](https://github.com/reddb-io/reddb).
The chart uses one container image and selects the deployment shape through
Kubernetes values rendered into `red` args and env vars.

## TL;DR

```bash
# Standalone dev/demo writer
helm install reddb ./charts/reddb

# Serverless writer with a mounted config file and remote backend values
helm install reddb ./charts/reddb \
  --set mode=serverless \
  --set remote.enabled=true \
  --set remote.backend=fs \
  --set remote.fs.path=/data/remote \
  --set serverless.lease.required=true

# Primary plus three read replicas
helm install reddb ./charts/reddb \
  --set mode=primary-replica \
  --set replica.replicaCount=3

# Symmetric cluster shape
helm install reddb ./charts/reddb \
  --set mode=cluster \
  --set cluster.replicaCount=3
```

## Topologies

| `mode` | Runtime shape | Storage preset |
|---|---|---|
| `standalone` | One writer StatefulSet, `red server --role standalone`. Default for local/dev. | `embedded` |
| `serverless` | One writer StatefulSet, local cache/PVC plus optional remote backend and lease. | `serverless` |
| `primary-replica` | One primary StatefulSet plus optional replica StatefulSet. Replicas run `red replica --primary-addr ...`. | `primary-replica-production-ha` |
| `cluster` | One symmetric StatefulSet with stable pod identity and headless discovery. Current CLI still runs `red server --role standalone` with the cluster storage preset. | `cluster` |

Set `storage.preset` to override the mode-derived preset. Use
`storage.profile`, `storage.packaging`, `storage.managedBackup`, and
`storage.walRetention` only when you intentionally need lower-level overrides.

## Rendered Resources

| Mode | StatefulSets | Services | Optional |
|---|---|---|---|
| `standalone` | `<release>-primary` | `<release>-primary` + headless | Ingress, NetworkPolicy, ServiceMonitor, Auth Secret |
| `serverless` | `<release>-primary` | `<release>-primary` + headless | Remote backend env, lease env, config file ConfigMap |
| `primary-replica` | `<release>-primary`; `<release>-replica` when `replica.replicaCount > 0` | Primary service; replica service when replicas exist | Replica PDB, NetworkPolicy primary<-replica |
| `cluster` | `<release>-cluster` | `<release>-cluster` + headless | Cluster PDB, peer discovery env |

## Config Precedence

There are two configuration classes:

- Boot/topology config is read before the database opens and must come from
  args/env: process role, data path, storage preset/profile, primary address,
  remote backend, lease settings, and secret material.
- Runtime config lives in `red.config` after boot.

`config.file` mounts JSON at `/etc/reddb/config.json` and writes missing keys
into `red.config` with write-if-absent semantics. Existing rows from a prior
boot, `SET CONFIG`, or boot defaults are not overwritten. Env overrides for
config-matrix keys still win for the current boot and are not persisted.

Use the config file for first-boot defaults and a small hot-reloadable set. Use
`SET CONFIG` or a migration when a stored value must change.

```yaml
config:
  file:
    enabled: true
    inline:
      red:
        logging:
          level: info
          format: json
      slow_query:
        threshold_ms: 500
```

To mount an existing ConfigMap instead:

```yaml
config:
  file:
    enabled: true
    existingConfigMap: reddb-config
    key: config.json
```

## Remote Backend

The chart emits the current cloud-agnostic env names:

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
  lease:
    required: true
```

Use `config.extraEnv` and `extraSecretMounts` for backend-specific knobs not yet
typed in the chart.

`mode` renders the human bootstrap env contract (`REDDB_TOPOLOGY` and
`REDDB_NODE_ROLE`) plus the storage env. The chart also renders
`REDDB_CONFIG_FILE` when `config.file.enabled` is set. The `red` binary consumes
that layer when explicit args are absent; explicit args still win, and
`storage.*` values override topology-derived storage defaults.

## Primary-Replica

`primary-replica` mode always renders one primary. Set
`replica.replicaCount=0` for a primary-only deployment that still uses the
primary-replica storage profile.

```yaml
mode: primary-replica
replica:
  replicaCount: 3
replication:
  commitPolicy: quorum
pdb:
  enabled: true
  minAvailable: 2
```

Replicas wait for the primary HTTP health endpoint before starting
`red replica --primary-addr http://<primary>:50051`.

## Cluster

Cluster mode renders stable StatefulSet identities and `REDDB_CLUSTER_PEERS`.
The current `red` CLI does not expose a distinct `cluster` process role, so the
pods run `red server --role standalone` with `REDDB_STORAGE_PRESET=cluster` and
`RED_CLUSTER_HA_INTENT=declared`. Treat this as the Kubernetes contract for the
cluster supervisor and range ownership runtime as those pieces mature.

## Security

- Non-root UID/GID `10001` by chart default.
- `readOnlyRootFilesystem: true`, all capabilities dropped, `RuntimeDefault` seccomp.
- `automountServiceAccountToken: false` by default.
- Auth bootstrap supports chart-managed or existing Secrets.
- Vault certificates can be injected through env or mounted file using the
  existing `auth.vault.certificate.fileMount` path.

## Uninstall

```bash
helm uninstall reddb
kubectl delete pvc -l app.kubernetes.io/instance=reddb
```
