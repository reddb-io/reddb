# RTO and RPO — what RedDB actually promises

This page tells operators what to expect when a RedDB database fails
or has to be rolled back: how long recovery takes (**RTO**, recovery
time objective) and how much data the recovery may lose (**RPO**,
recovery point objective). The numbers reflect the engine's current
durability story (WAL + snapshot archiving + PITR replay) and assume
the deployment shape from
[`docs/security/vault.md`](../security/vault.md) and
[`docs/operations/secrets.md`](secrets.md).

If you came here looking for backup configuration, see the
[backup section in the README](../../README.md#backup--recovery)
first.

---

## TL;DR

| Failure mode | RTO target | RPO target |
|---|---|---|
| Process crash, disk intact | seconds | zero (committed-up-to-WAL-fsync) |
| Disk loss, recover from local snapshot + WAL | seconds–minutes | zero (committed-up-to-WAL-fsync) |
| Disk loss, recover from remote snapshot + WAL | minutes (depends on snapshot size) | seconds (last archived WAL segment) |
| Application-level rollback to a target time | minutes | bound by archive cadence |
| Replica promoted after primary loss | seconds | bounded by replication lag |

Numbers further down are concrete.

---

## What RedDB actually persists

RedDB's durability model has three layers, in order of how much they
contribute to RPO:

1. **WAL**. Every write goes to the write-ahead log before the
   in-memory store reports success. `wal::sync()` fires on every
   commit. RPO inside this layer = whatever the OS holds in
   page-cache between `fsync` calls. With default settings, **a
   process crash on a live disk loses zero committed transactions**.
   See `src/storage/wal/transaction.rs::commit`.

2. **Snapshots**. Periodic checkpoints of the in-memory store, written
   as a `.rdb-snapshot` blob. Combined with the WAL since the
   snapshot's base LSN, a snapshot is a fast-rewind point for PITR.
   See `src/storage/wal/checkpoint.rs`.

3. **Remote archive**. WAL segments and snapshots are uploaded to a
   remote backend (S3, R2, GCS, Turso, D1, or local fs). A disk loss
   on the primary recovers from the archive — RPO is bounded by the
   archive cadence. See `src/storage/wal/archiver.rs`.

When sizing RTO/RPO budgets, decide first which of these layers
survives the failure scenario you care about, then read off the row
in the table below.

---

## Numbers

The table below is calibrated for a single-node primary. Adjust by
size class; the engine's durability path is `O(WAL replay)` once the
snapshot is open, so RTO scales linearly with the unflushed WAL volume,
not with the total database size.

### Crash without disk loss

The local disk still has WAL + snapshot. Recovery is:

1. Open the `.rdb` file → header + meta-shadow recovery
2. Replay WAL records since the last checkpoint
3. Resume serving

Order-of-magnitude for typical workloads:

| Database size | Unflushed WAL window | Cold-start RTO |
|---|---|---|
| 100 MB | 10 MB | < 1 s |
| 1 GB   | 50 MB | 1–2 s |
| 10 GB  | 100 MB | 3–5 s |
| 100 GB | 200 MB | 10–20 s |
| 1 TB   | 200 MB | 30–60 s |

RPO under crash-without-disk-loss is **zero** for committed
transactions: WAL `fsync` is the durability boundary on commit, not a
cache flush.

### Disk loss, recover from remote archive

The local disk is gone (volume failure, container destroyed, etc.).
RedDB pulls the latest snapshot + WAL segments from the configured
backend. Recovery is:

1. Download latest snapshot manifest
2. Download snapshot blob
3. Download WAL segments since the snapshot's base LSN
4. Replay WAL records, validate hash chain
5. Open recovered `.rdb`

RTO is dominated by the snapshot download. WAL replay is fast (each
segment is sized so replay completes in under a second).

| Database size | Snapshot blob | RTO @ 100 Mbit/s | RTO @ 1 Gbit/s |
|---|---|---|---|
| 100 MB | ~80 MB | ~7 s | < 1 s |
| 1 GB | ~700 MB | ~60 s | ~6 s |
| 10 GB | ~7 GB | ~10 min | ~1 min |
| 100 GB | ~70 GB | ~100 min | ~10 min |
| 1 TB | ~700 GB | ~16 h | ~100 min |

For deployments that need sub-minute RTO at large sizes, run a
replica fleet — promotion is bounded by replication lag, not snapshot
download. See the [replication section of the README](../../README.md).

RPO is bounded by the archive cadence:

| Archive cadence | Worst-case RPO |
|---|---|
| `wal_archive_interval = 1s` (default) | 1 s |
| 10 s | 10 s |
| 60 s | 60 s |

### Application-level rollback to a target time

Use case: an operator command corrupted data at `t_bad`. Roll back
to `t_bad - epsilon`. Recovery is:

1. Pick the latest snapshot whose `snapshot_time <= t_bad - epsilon`
2. Download it
3. Replay WAL up to but not past the target time
4. Open recovered `.rdb`

RTO is the same as a remote-restore (same downloads + replay). RPO is
the time gap between the target and the last applied WAL record —
because RedDB stops replay at the first record past the target, there
is no data loss within the target window.

The drill that proves this works:
[`tests/drill_pitr_target_time.rs`](../../tests/drill_pitr_target_time.rs).

### Replica promoted after primary loss

This is the lowest-RTO option. The replica was already replaying the
primary's WAL stream; promotion is mostly a handshake with the lease
service.

| Replication lag (typical) | Promotion RTO | RPO |
|---|---|---|
| ms | seconds | replication lag |
| seconds | seconds | replication lag |
| minutes (slow replica) | seconds | replication lag |

The drill that proves promotion fails-closed when the replica's chain
is broken:
[`tests/chaos_promote_refused_when_lease_held.rs`](../../tests/chaos_promote_refused_when_lease_held.rs).

---

## Verifying the contract on your build

Three drills that run in CI pin the recovery contract:

- [`tests/drill_backup_restore_round_trip.rs`](../../tests/drill_backup_restore_round_trip.rs)
  — full archive → simulated primary loss → restore round trip.
- [`tests/drill_pitr_target_time.rs`](../../tests/drill_pitr_target_time.rs)
  — PITR target-time semantics (records after target are not applied).
- [`tests/drill_pitr_byte_identical.rs`](../../tests/drill_pitr_byte_identical.rs)
  — restored DB collection inventory matches the snapshot's.

If you are about to ship a release that touches the WAL, snapshot
serialization, or archive layout, rerun all three drills locally and
verify CI green before tagging.

---

## What RedDB does NOT promise

- **Disk corruption beyond CRC32.** RedDB checksums every page and
  WAL record with CRC32 and refuses to load a corrupted page. It does
  not silently repair. Recovery from disk corruption goes through the
  remote archive path; if the remote is also corrupt, you lose data.
- **Distributed two-phase commit.** Replication is single-primary
  with eventual fan-out to replicas. There is no consensus layer; a
  network split that elects two primaries is prevented by the lease
  service, not by the storage engine.
- **Sub-second RTO at multi-TB scale without a replica fleet.** A
  cold restore at TB scale is dominated by network throughput; a hot
  replica is the right tool.
