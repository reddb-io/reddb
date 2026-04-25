# Scaling RedDB: Primary + Read Replicas

PLAN.md Phase 11.1. The first production-grade topology is **single writer + N async read replicas**. Synchronous quorum is opt-in (`ack_n`) once chaos-tested.

## Topology

```
                   ┌──────────────┐
   writes  ───────▶│   primary    │
                   │  (leases)    │──┐
                   └──────┬───────┘  │ remote backend
                          │          │ (S3, R2, FS, HTTP)
            WAL pull RPC  │          ▼
       ┌──────────────────┼─────────────────────────┐
       ▼                  ▼                          ▼
  ┌──────────┐      ┌──────────┐               ┌──────────┐
  │ replica1 │      │ replica2 │     ...       │ replicaN │
  └──────────┘      └──────────┘               └──────────┘
       ▲                                            ▲
       └────────── stale reads ──── load balancer ─┘
```

Exactly one primary holds the writer lease (PLAN.md Phase 5.2 / 11.2). Replicas pull WAL records via the gRPC `pull_wal_records` RPC and ack via `ack_replica_lsn`.

## Read semantics

| Path | Guarantee |
|------|-----------|
| Read on primary | Read-your-writes (linearizable). |
| Read on replica | Stale (eventual consistency). Replicas may serve data that's milliseconds-to-seconds behind primary. |
| Read on replica with `applied_lsn >= required_lsn` check | Read-your-writes when the client passes `required_lsn` (Phase 11.1 readiness contract; client SDK helper). |

Every response can carry an `observed_lsn` for clients that need to wait for catch-up.

## When to add a replica

| Symptom | Diagnosis | Action |
|---------|-----------|--------|
| Read CPU saturating primary | Read fan-out exceeds primary capacity | Add a replica behind a read-only DNS / load balancer. |
| Backup window stretches into peak hours | Primary's I/O competes with reads | Move scheduled exports to a replica. |
| Need cross-region read latency | Replica close to readers | Deploy replica in target region with `region` field in handshake. |
| Need DR posture | Survive primary loss | At least 2 replicas in distinct failure domains. |

## When NOT to add a replica

- Write-heavy workload — replicas don't help; they increase WAL fanout cost on primary.
- Sub-second consistency requirement — async replication can drift. Use primary directly or wait on `applied_lsn`.
- Strong durability required — set `RED_PRIMARY_COMMIT_POLICY=ack_n=N` so commits block until N replicas durable. **Increases write latency**; budget accordingly.

## Lag monitoring

Per-replica metrics on the primary:

```
reddb_replica_count
reddb_replica_ack_lsn{replica_id="..."}
reddb_replica_lag_records{replica_id="..."}     # current_lsn - last_acked_lsn
reddb_replica_lag_seconds{replica_id="..."}     # now - last_seen_at
reddb_replica_apply_errors_total{kind="gap|divergence|apply|decode"}
```

Alert thresholds (suggested, tune per workload):

| Metric | Warn | Crit |
|--------|------|------|
| `reddb_replica_lag_records` | 1000 | 100000 |
| `reddb_replica_lag_seconds` | 30s | 300s |
| `reddb_replica_apply_errors_total{kind="divergence"}` | 0 (any) | 0 (any) — page immediately |

Replica-side `apply_health` is in `/admin/status` under `replica.apply_health`. Possible values: `ok`, `healthy`, `connecting`, `stalled_gap`, `divergence`, `apply_error`.

## Bootstrapping a new replica

1. Provision the volume.
2. Set `RED_REPLICATION_MODE=replica` + `RED_PRIMARY_ADDR=http://<primary>:50051`.
3. If a recent snapshot is in the remote backend, set `RED_AUTO_RESTORE=true` so the replica seeds from there before catching up via WAL pull.
4. Boot. Watch `replica.apply_health` flip to `connecting` then `ok`.
5. Add to the read load balancer once `ok` and `lag_seconds < threshold`.

## Commit policies

Set via `RED_PRIMARY_COMMIT_POLICY` (default: `local`):

| Policy | Behavior |
|--------|----------|
| `local` | Commit returns after local WAL fsync. Default. No replica blocking. |
| `remote_wal` | (declared, not yet enforced) Commit returns after WAL segment archived to remote. |
| `ack_n=N` | Commit returns after N replicas ack. Currently wired in `trigger_backup`; per-DML wiring is in progress. Tune `RED_REPLICATION_ACK_TIMEOUT_MS` (default 5000) and `RED_COMMIT_FAIL_ON_TIMEOUT` (default false → log+continue, true → 504). |
| `quorum` | (declared, not yet enforced) Future: hooks into `QuorumCoordinator`. |

**Don't claim "synchronous replication" in marketing until `ack_n` is wired into every public mutation surface and chaos-tested.**

## Region awareness

Replicas declare their region at handshake (`region` field). Tracked per-replica in `/admin/status`. Future quorum policies can require N-of-M from distinct regions; today this is informational.

## Failover

See [`runbook.md` §3](runbook.md#3-failover-and-promotion). RedDB v1 ships **manual** promotion only. Automatic promotion needs:

- A consensus layer (or accepted leader-election service like etcd / Consul).
- Proven safety contracts under partition and clock-skew chaos.

Both planned post-v1.0.
