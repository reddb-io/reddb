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

`mode` renders the human topology env contract (`REDDB_TOPOLOGY` and
`REDDB_NODE_ROLE`) plus the storage env. The chart also renders
`REDDB_CONFIG_FILE` when `config.file.enabled` is set. Process roles still come
from the chart-rendered args, while `storage.*` values override mode-derived
storage defaults.

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

### Cluster bootstrap contract

Cluster members are symmetric and *non-owner*: no member can prove it — and not
a peer — is the single writer of global auth/vault/config/policy state. The
chart and the runtime therefore share one fail-closed contract (ADR 0058,
[`docs/deployment/first-boot.md`](../../docs/deployment/first-boot.md)):

- **Cluster no-auth (supported today).** The default cluster values render no
  bootstrap credentials at all. Members boot anonymously and create no admin,
  vault, or `system.bootstrap.completed` marker. This is the documented
  development/no-auth carveout, not a credentialled production bootstrap.
- **Cluster auth/vault (gated).** `auth.enabled=true` is rejected fail-closed in
  `mode=cluster`: `helm template`/`helm install` fail with a message that points
  here. The gate stays closed in lockstep with the runtime, which rejects a
  cluster-shaped credentialled boot until the reserved global system range owner
  path lands (PRD #1227). When that owner path ships, only the proven reserved
  range owner runs the first preset, mints the vault, applies the first policy
  manifest, and publishes the completion marker.
- **Certificate handling.** A cluster member may still receive the vault
  certificate (`auth.vault.certificate.value` / `existingSecret`, or the
  `fileMount` path) so it can *unseal* an already-bootstrapped store. Receiving a
  certificate never bootstraps auth — it only opens an existing vault. Capture
  the certificate the owner mints once and preserve it offline; losing it means
  the encrypted store cannot be unsealed.
- **Restart idempotency.** Once `system.bootstrap.completed` is durable, a
  restart observes the marker and rehydrates read-only state: it recreates no
  admin, reissues no certificate, and reapplies no mutable config over operator
  changes. Re-running `helm upgrade` is safe for the same reason.
- **Non-owner behavior.** Because members never receive bootstrap credentials,
  the bootstrap env (`REDDB_PRESET`, `REDDB_USERNAME`, `REDDB_PASSWORD`,
  `REDDB_BOOTSTRAP_MANIFEST`) is never rendered into a cluster pod. A non-owner
  that did observe credentials must wait for / redirect to the owner rather than
  mutate global auth state.

`scripts/verify-helm-chart.sh` asserts this contract on every render: cluster
pods carry no bootstrap credentials, `auth.enabled=true` fails closed, and a
vault certificate is still injectable for unseal.

## Security

- Non-root UID/GID `10001` by chart default.
- `readOnlyRootFilesystem: true`, all capabilities dropped, `RuntimeDefault` seccomp.
- `automountServiceAccountToken: false` by default.
- Auth bootstrap supports chart-managed or existing Secrets. When
  `auth.enabled=true`, `REDDB_PRESET=production`, `REDDB_USERNAME`, and
  `REDDB_PASSWORD` are rendered only into the writer pod: serverless or primary.
  Replica pods never receive bootstrap credentials, and `mode=cluster` rejects
  chart-managed auth bootstrap fail-closed until the reserved global system
  range owner path lands (see [Cluster bootstrap contract](#cluster-bootstrap-contract)).
- Vault certificates can be injected through env or mounted file using the
  existing `auth.vault.certificate.fileMount` path.
- `auth.vault.bootstrapJob.enabled` is disabled fail-closed. The legacy hook
  bootstrapped an `emptyDir` database, not the writer PVC, so its certificate did
  not belong to the real database. Run `red bootstrap` against the real writer
  volume or use HTTP bootstrap after the writer starts.

## Uninstall

```bash
helm uninstall reddb
kubectl delete pvc -l app.kubernetes.io/instance=reddb
```
