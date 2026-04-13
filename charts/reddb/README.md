# RedDB Helm Chart

A production-grade Helm chart for [RedDB](https://github.com/forattini-dev/reddb) — a unified multi-model database engine (tables, documents, graphs, vectors, key-value).

## TL;DR

```bash
# Single-node (standalone)
helm install reddb ./charts/reddb

# Single primary + 3 read replicas
helm install reddb ./charts/reddb \
  --set mode=primary-replica \
  --set replica.replicaCount=3
```

## Topologies

| `mode`            | Description                                                                                          |
|-------------------|------------------------------------------------------------------------------------------------------|
| `standalone`      | One StatefulSet, one pod, role `standalone`. Reads + writes on the same node. Default.               |
| `primary-replica` | One primary StatefulSet (1 pod, single writer) plus a replica StatefulSet (`replica.replicaCount` read-only pods streaming via gRPC). |

Replicas wait for the primary to become healthy via an init container before starting `red replica --primary-addr ...`.

## Resources rendered

| Mode              | StatefulSets               | Services (ClusterIP + headless) | Optional                                                |
|-------------------|----------------------------|---------------------------------|---------------------------------------------------------|
| `standalone`      | `<release>-primary`        | `<release>-primary` + headless  | Ingress, NetworkPolicy, ServiceMonitor, Auth Secret     |
| `primary-replica` | `+ <release>-replica`      | `+ <release>-replica` + headless | + PodDisruptionBudget on the replica set                |

## Endpoints

After install:

- Primary writer (gRPC + HTTP): `<release>-primary.<namespace>.svc.cluster.local`
- Read replicas (gRPC + HTTP): `<release>-replica.<namespace>.svc.cluster.local`

Default ports: `50051` (gRPC), `8080` (HTTP). The HTTP port is also used by liveness/readiness probes via `red health`.

## Persistence

Each pod gets a `data` PVC (default `10Gi`, `ReadWriteOnce`) mounted at `/data`. The DB file lives at `/data/data.rdb`. Configure via `primary.persistence.*` and `replica.persistence.*`.

To run ephemerally for testing, set `primary.persistence.enabled=false` (data lives in an `emptyDir`).

## Auth bootstrap

Enable the chart-managed Secret to auto-create the first admin on startup:

```yaml
auth:
  enabled: true
  username: admin
  password: change-me-please
```

Or reference an existing Secret with keys `username` / `password`:

```yaml
auth:
  enabled: true
  existingSecret: reddb-admin
```

## Security

- Non-root UID/GID `10001` (matches the Dockerfile).
- `readOnlyRootFilesystem: true`, all capabilities dropped, `RuntimeDefault` seccomp.
- `automountServiceAccountToken: false` by default.
- Optional NetworkPolicy locks ingress to: replicas → primary, plus user-supplied selectors.

## Common values

| Key                              | Default                              | Notes                                 |
|----------------------------------|--------------------------------------|---------------------------------------|
| `mode`                           | `standalone`                         | `standalone` or `primary-replica`     |
| `image.repository`               | `ghcr.io/forattini-dev/reddb`        |                                       |
| `image.tag`                      | `""` (chart `appVersion`)            |                                       |
| `primary.persistence.size`       | `10Gi`                               |                                       |
| `replica.replicaCount`           | `2`                                  | Only used in `primary-replica`        |
| `replica.persistence.size`       | `10Gi`                               |                                       |
| `pdb.enabled`                    | `false`                              | Applies only to the replica set       |
| `networkPolicy.enabled`          | `false`                              |                                       |
| `ingress.enabled`                | `false`                              | HTTP only                             |
| `metrics.serviceMonitor.enabled` | `false`                              | Requires prometheus-operator CRDs     |
| `auth.enabled`                   | `false`                              | Bootstrap first admin user            |

See `values.yaml` for the full list.

## Examples

### Standalone with persistence and ingress

```yaml
mode: standalone
primary:
  persistence:
    size: 50Gi
    storageClass: gp3
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: reddb.example.com
      paths:
        - path: /
          pathType: Prefix
          backend: primary
```

### HA-ish reads: 1 primary + 3 replicas with PDB and NetworkPolicy

```yaml
mode: primary-replica
replica:
  replicaCount: 3
  persistence:
    size: 50Gi
    storageClass: gp3
pdb:
  enabled: true
  minAvailable: 2
networkPolicy:
  enabled: true
  ingressFrom:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: apps
```

## Uninstall

```bash
helm uninstall reddb
# PVCs are NOT deleted by Helm — clean them up explicitly if needed:
kubectl delete pvc -l app.kubernetes.io/instance=reddb
```
