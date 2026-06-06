# Storage Profiles

Storage profiles describe the physical packaging RedDB uses for a deployment.
They are separate from the query API and transport mode: the same logical
collections can run in embedded, serverless, primary-replica, or cluster shape,
but the files operators must back up, retain, and move are different.

Runtime selection is visible through `SHOW CONFIG`:

```sql
SHOW CONFIG storage.deploy.profile;
SHOW CONFIG storage.deploy.packaging;
SHOW CONFIG storage.deploy.preset;
SHOW CONFIG storage.deploy.managed_backup;
SHOW CONFIG storage.deploy.wal_retention;
```

## Profile Matrix

| Deployment profile | Packaging | Physical shape | Recovery boundary |
|---|---|---|---|
| `embedded` | `single-file` | One canonical `.rdb` opened by the application. Required local durability sidecars, such as WAL, stay adjacent to the data file when enabled. | Local file backup or snapshot plus the adjacent WAL needed for crash recovery. |
| `serverless` | `operational-directory` plus segment pack artifacts | A local hydrated `.rdb` for serving, remote snapshots/WAL for cold restore, and immutable segment packs for checkpointed byte distribution. | Segment packs currently declare `recovery_boundary.kind = "checkpointed-rdb"` and `wal_segments_required = 0`; remote restore uses the backup manifest and WAL archive. |
| `primary-replica` | `single-file` for dev/small presets, `operational-directory` for production backup, WAL retention, or more than one replica | Primary and each replica keep local data files plus logical WAL spool and replication metadata. Production presets move toward an operational directory so snapshots, WAL, and metadata are explicit. | Primary snapshot plus retained WAL/logical spool. Replicas rebootstrap from a snapshot, then catch up from retained logical WAL records. |
| `cluster` | `operational-directory` | Range-directory layout under the support directory. Each collection range has stable logical range identity, range metadata, and per-range data/index/append segment files. | Range snapshots plus range-stamped WAL records. Full online range movement and rebalancing remain roadmap work. |

## Embedded Single-File

Embedded mode is the default profile and preserves the v1 single-file contract:
`RedDB::open("./data.rdb")` owns the canonical data file. This is the packaging
operators should choose for local-first applications, CLIs, desktop apps, and
single-process services that want the smallest artifact set.

The file format stability and migration rules are governed by
[ADR 0003](../../.red/adr/0003-disk-format-v1.md). Tiered path derivation and
support directory naming are defined by
[ADR 0018](../../.red/adr/0018-tiered-storage-layout.md). ADR 0018 does not move
every callsite by itself; it defines the layout vocabulary that later runtime
wiring consumes.

## Serverless Segment Pack

Serverless deployments serve from a local hydrated file but treat remote storage
as the durability and cold-start boundary. The remote backend stores snapshots,
WAL segments, `MANIFEST.json`, and the writer lease when CAS is available. See
[Remote Backends](backends.md) for the conditional-write contract.

Segment packs are a separate distribution artifact for checkpointed `.rdb`
bytes. A pack contains immutable `parts/` plus `manifest.json`, with checksums
for every part and a recovery boundary that currently requires no WAL replay
beyond the checkpointed source file. Hydration validates the manifest and parts
before writing the destination `.rdb`.

Do not treat segment packs as an online compaction or PITR mechanism. They are a
checkpointed byte packaging format for serverless cold-start distribution. PITR
still depends on the backup manifest and WAL archive.

## Primary-Replica Operational Layout

Primary-replica deployments are timeline-oriented, not just "copy the file to
another node." The primary writes the local physical WAL and exposes logical WAL
records for replicas. Replicas bootstrap from a snapshot, then apply logical WAL
records until they catch the primary's advertised frontier.

The current presets intentionally allow `single-file` for development and small
topologies, but production shapes that enable managed backup, WAL retention, or
more than one replica require `operational-directory`. That keeps the snapshot,
WAL, and replication metadata boundary visible to operators.

The consistency and failover contract is defined by
[ADR 0030](../../.red/adr/0030-replication-consistency-and-failover-model.md).
Causal read bookmarks depend on the same commit watermark in
[ADR 0031](../../.red/adr/0031-causal-consistency-bookmarks-and-ttl-replication.md).
The physical WAL is the source of truth for derived logical replication records
per [ADR 0032](../../.red/adr/0032-wal-source-of-truth-and-term-framing.md).

## Cluster Range Layout

Cluster profile uses `operational-directory` packaging and creates a
`range-directory` layout under the data file support directory. The layout gives
each collection range a logical range id, a physical range directory id,
`range.meta`, `data.rdb`, `index.rdb`, and `segments.aof`.

This is the physical packaging foundation for future cluster range ownership and
movement. The current supported surface is range metadata, range-stamped WAL
identity, and snapshot install/catch-up plumbing. Automatic shard balancing,
multi-region consensus, and arbitrary online conversion from local stores to a
running cluster are out of scope for the current profile contract.

## Migration Paths

The first supported storage-profile migration is offline embedded single-file to
operational directory:

```rust
use reddb::RedDBOptions;
use reddb::storage::operational_migration::migrate_embedded_to_operational;

migrate_embedded_to_operational("./data.rdb", "./data-operational")?;
let options = RedDBOptions::operational_directory("./data-operational");
```

This is a closed source file export. The migrator takes the same exclusive lock
as the embedded runtime, validates the checkpoint header and page checksums,
writes `MANIFEST.json`, and copies stable file identities under `files/`.

Unsupported directions in the current contract:

- Operational directory back to embedded single-file.
- Online conversion while the source database is open.
- Primary-replica to embedded rollback, except by restoring the original backup
  or preserved source `.rdb`.
- Serverless segment pack as a replacement for backup/PITR WAL replay.
- Embedded or primary-replica store converted directly into a live cluster range
  layout.

## Backup, Restore, and WAL Retention

For local and primary-replica stores, backup safety is bounded by the latest
validated snapshot plus every WAL segment needed to reach the requested restore
point. Restore validates snapshot checksums and the WAL hash chain; a missing,
corrupt, or reordered segment fails restore rather than producing a partial
database.

For primary-replica deployments, WAL retention is also a replication health
boundary. A replica that falls behind the retained WAL window reports a gap and
must be re-bootstrapped from a snapshot that covers the missing range. Widen
retention before maintenance windows that may pause replicas.

For serverless segment packs, the manifest's recovery boundary is narrower:
checkpointed bytes only, with `wal_segments_required = 0`. Use remote backup and
WAL archive restore for PITR or crash recovery beyond that checkpoint.

## See Also

- [Embedded Mode](embedded.md)
- [Serverless Mode](serverless.md)
- [Replication](replication.md)
- [Remote Backends](backends.md)
- [Operator Runbook](../operations/runbook.md)
- [Native Migrations](../migrations/overview.md#offline-storage-profile-migration)
