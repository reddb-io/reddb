# RedDB Operator Runbook

Practical playbook for running RedDB in production. Pairs with:

- [`docs/spec/admin-api.openapi.yaml`](../spec/admin-api.openapi.yaml) — endpoint reference.
- [`docs/spec/manifest-format.md`](../spec/manifest-format.md) — backup catalog format.
- [`docs/spec/wal-format.md`](../spec/wal-format.md) — WAL on-disk + archived format.
- [`examples/`](../../examples/) — Kubernetes, Docker Compose, Fly Machines, systemd manifests.

---

## 1. Deploy

### Single-node (smallest viable)

Pick any one of the reference manifests:

```bash
docker compose -f examples/docker-compose.universal.yml up -d
# or
fly deploy --config examples/fly/fly.toml
# or
kubectl apply -f examples/kubernetes/reddb.yaml
```

Required env minimum:

| Variable | Purpose |
|----------|---------|
| `RED_HTTP_BIND_ADDR` | data-plane bind, e.g. `0.0.0.0:8080` |
| `RED_BACKEND` | `s3`, `fs`, `http`, or `none` |
| `RED_BACKEND` keys | `RED_S3_*` / `RED_FS_PATH` / `RED_HTTP_BACKEND_URL` per backend |
| `RED_AUTO_RESTORE` | `true` to rebuild from remote on empty volume |
| `RED_BACKUP_ON_SHUTDOWN` | `true` to take a final backup on SIGTERM |

Sensitive values use the `_FILE` companion convention (Kubernetes Secret, Docker secrets, systemd `LoadCredential`):

```yaml
env:
  - name: RED_S3_SECRET_KEY_FILE
    value: /run/secrets/s3-secret
  - name: RED_ADMIN_TOKEN_FILE
    value: /run/secrets/admin-token
```

The `_FILE` companion wins over the inline value when both are set.

### Writer lease backend matrix

Use `RED_LEASE_REQUIRED=true` only with a backend that supports conditional writes. S3-compatible stores use ETag + `If-Match`; filesystem uses a content-hash token plus an exclusive local lock. Generic HTTP is eligible only when the service implements ETag and precondition headers and the operator sets `RED_HTTP_CONDITIONAL_WRITES=true`.

| Runtime | fs lease | s3 lease | Notes |
|---------|----------|----------|-------|
| K8s + PVC RWO | ✅ recommended | ✅ | PVC must be mounted by one writer at a time. |
| Fly Machines + volume | ✅ recommended | ✅ | Volume follows the machine. |
| ECS + EBS | ✅ recommended | ✅ | Avoid shared EFS for writer lease. |
| Lambda + EFS (NFS) | ❌ flaky locks | ✅ required | Use S3-compatible CAS for fencing. |
| Nomad + host volume | ✅ | ✅ | Host volume must be pinned to the allocation. |
| Cloud Run | ❌ ephemeral | ✅ required | No durable fs lease. |
| App Runner | ❌ ephemeral | ✅ required | No durable fs lease. |
| Container Apps | ❌ ephemeral | ✅ required | No durable fs lease. |
| Generic HTTP backend | ❌ unless local durable disk | ✅ via backend service | Requires `RED_HTTP_CONDITIONAL_WRITES=true`. |

If the chosen backend cannot enforce conditional writes, RedDB refuses to acquire the lease and keeps the writer gate closed. That is intentional: a failed boot is safer than split-brain.

### Verify the deploy

```bash
red doctor --bind <host>:<port> --json
curl -s http://<host>:<port>/admin/status -H "Authorization: Bearer $RED_ADMIN_TOKEN" | jq
curl -s http://<host>:<port>/metrics -H "Authorization: Bearer $RED_ADMIN_TOKEN" | head
```

`red doctor` exits 0 healthy / 1 warn / 2 critical and is safe to run from CI gates and on-call playbooks.

---

## 2. Backup and Restore

### Trigger a backup

```bash
curl -X POST http://<host>:<port>/admin/backup \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN"
```

Returns `{ok, snapshot_id, uploaded, duration_ms}`. The unified `MANIFEST.json` at the configured `<prefix>/` is refreshed atomically; per-segment sidecars are published before the catalog updates.

### Restore from remote (cold start)

Empty `/data` + `RED_AUTO_RESTORE=true` triggers an automatic restore on boot. To force a restore manually:

```bash
# Put the runtime in read-only first so writes can't race the swap
curl -X POST http://<host>:<port>/admin/readonly \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"enabled": true}'

curl -X POST http://<host>:<port>/admin/restore \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"to_timestamp_ms": 1730000000000}'  # 0 = latest
```

Restore validates the snapshot SHA-256 and the WAL hash chain end-to-end. **A break aborts restore** with a typed error — the destination file is left in place for forensics.

### Verify a restore drill in CI

```bash
cargo test --test drill_backup_restore_round_trip
cargo test --test drill_pitr_target_time
cargo test --test drill_pitr_chain_break_within_window
```

---

## 3. Failover and Promotion

RedDB v1 ships **manual** promotion. Auto-promotion lives in a future release after the safety contract has been chaos-tested at scale.

### Choose the freshest replica

```bash
for HOST in replica-1 replica-2 replica-3; do
  echo -n "$HOST: "
  curl -sf "http://$HOST:8080/admin/status" \
    -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
    | jq '.wal.current_lsn // 0'
done
```

Pick the host with the highest `current_lsn`.

### Promote

```bash
curl -X POST http://<chosen>:8080/admin/failover/promote \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"holder_id": "promoter-runbook", "ttl_ms": 60000}'
```

The handler refuses (409) when:

- The instance isn't a replica.
- Apply health is `stalled_gap`, `divergence`, or `apply_error`.
- Another holder owns a fresh lease.

A 412 means no remote backend is configured (lease has nowhere to live).

### Finish the promotion

The endpoint **does not auto-flip the role** — it acquires the lease + audits. The runbook step is to restart the chosen replica with `RED_REPLICATION_MODE=primary`. Old primary, when it comes back, must boot as a replica or refuse to start (operator policy).

---

## 4. Secret Rotation

All sensitive env vars accept a `_FILE` companion. The runtime reads the file once at boot. To rotate without restart, today's path is:

1. Update the secret in your KMS / Kubernetes Secret / Docker secret.
2. Roll the pod / restart the systemd unit. RedDB takes a graceful-shutdown backup if `RED_BACKUP_ON_SHUTDOWN=true`.

In-flight SIGHUP reload of secrets is a Phase 6.4 follow-up.

### Admin token rotation

```bash
# Generate a new token
NEW=$(openssl rand -hex 32)
echo -n "$NEW" > /run/secrets/admin-token
# Restart with the new file. Old token immediately invalid.
```

---

## 5. Monitoring

### `red doctor` thresholds

```bash
red doctor \
  --bind <host>:<port> \
  --token "$RED_ADMIN_TOKEN" \
  --backup-age-warn-secs 600 \
  --backup-age-crit-secs 3600 \
  --wal-lag-warn 1000 \
  --wal-lag-crit 10000 \
  --json
```

Exit codes:

- `0` — healthy, no checks fired.
- `1` — at least one warn (backup older than warn threshold, read-only flag set, etc.).
- `2` — at least one critical (lease lost, divergence, server unreachable, sha256 mismatch).

### Key metrics to alert on

| Metric | Alert when | Why |
|--------|-----------|-----|
| `reddb_health_status` | `< 2` for 5m | Engine isn't ready. |
| `reddb_writer_lease_state{state="not_held"}` | == 1 | Split-brain risk if role is primary. |
| `reddb_backup_age_seconds` | `> 3600` | DR posture degraded. |
| `reddb_wal_archive_lag_records` | `> 10000` | Archive stuck (S3 down? credentials wrong?). |
| `reddb_replica_apply_errors_total{kind="divergence"}` | `> 0` | Corruption / split-brain — page operator now. |
| `reddb_replica_apply_errors_total{kind="gap"}` | rising | Replica is unhealthy; consider rebootstrap. |
| `reddb_replica_lag_records` | `> 100000` | Replica too far behind to be promoted safely. |
| `reddb_slo_lag_budget_remaining_seconds` | `< 0` | Replica lag exhausted `RED_SLO_REPLICA_LAG_BUDGET_SECONDS`. |
| `reddb_commit_wait_total{outcome="timed_out"}` | rising | `ack_n` policy is too tight or replicas can't keep up. |
| `reddb_quota_rejected_total{principal=...}` | sustained | Caller exceeded `RED_MAX_QPS_PER_CALLER` budget. |

### Dashboards

Standard Prometheus scrape; metrics exposition format `text/plain; version=0.0.4`. The same metrics are surfaced in `/admin/status` as JSON for systems that prefer pull-once over scrape.

---

## 6. Upgrades

### Patch versions (engine 0.x.y → 0.x.z)

Drop-in. Stop the old pod / restart the systemd unit. WAL format and manifest format are stable across patches.

### Minor versions

Read the release notes for any spec major bumps. The unified `MANIFEST.json` carries `engine_version` so an external verifier can refuse to operate on incompatible catalogs.

### Coordinated upgrade with replicas

1. Upgrade replicas first, one at a time. Wait for `replica.apply_health == ok` after each.
2. Promote a known-good replica (Section 3).
3. Upgrade the old primary; bring it back as a replica.

### Panic policy

Release binaries are built with `panic = "abort"`. RedDB treats unexpected panic as process-fatal because unwinding through write, recovery, or replication paths can leave in-memory state inconsistent with the WAL. The recovery contract is: crash fast, let the supervisor restart, replay WAL, and fail closed if WAL/hash-chain validation detects corruption.

Do not wrap engine write paths in broad `catch_unwind` as an availability shortcut. RPC/server boundaries may convert isolated request panics into 500 responses only when the mutation did not enter the storage path.

---

## 7. Common Failure Modes

| Symptom | Diagnosis | Fix |
|---------|-----------|-----|
| `restore failed: chain` | A WAL segment was deleted, replaced, or reordered. | Inspect bucket history; restore to a target_time before the break. |
| `restore failed: integrity` | sha256 mismatch on a snapshot or segment. | Check object-store-side replication / versioning; pick a different snapshot_id. |
| `lease not held — DML rejected` | Heartbeat lost contact with backend (S3 down, lease prefix wrong). | Check `RED_LEASE_PREFIX`, confirm backend reachable, restart. The instance correctly stopped writing — no data loss. |
| Replica `apply_health == divergence` | Same LSN with different payload — corruption or split-brain. | **Stop applying immediately**. Re-bootstrap the replica from the freshest snapshot. Investigate primary integrity. |
| Replica `apply_health == stalled_gap` | Replica is behind the primary's oldest available WAL. | Re-bootstrap from a snapshot covering the gap. |
| `429 rate limited` from `/admin/*` | Per-caller quota exhausted. | Inspect `reddb_quota_rejected_total{principal=…}`; raise `RED_MAX_QPS_PER_CALLER` or shed load. |
| Cold start `>10s` | Volume is empty + restore pulling many WAL segments. | Increase `RED_BACKUP_INTERVAL_SECS` so snapshots are fresher → fewer WAL segments to replay. |

---

## 8. Disaster Recovery Drill

Run quarterly:

1. Pick a non-prod cluster.
2. Snapshot current state (record current_lsn, last backup timestamp).
3. Stop the primary (`docker stop`, `kubectl delete pod`, or `systemctl stop`).
4. Wipe `/data`.
5. Bring the pod back up with `RED_AUTO_RESTORE=true`.
6. Verify post-restore: `red doctor` exits 0, `current_lsn` matches what was archived, sample row reads return expected payloads.
7. Document the drill outcome (duration, any warns, any data loss).

The chaos test suite (`tests/chaos_*` and `tests/drill_*`) covers the unhappy paths automatically; the manual drill covers the operator workflow + integration with your environment.
