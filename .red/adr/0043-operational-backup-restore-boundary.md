# Operational Backup and Restore Boundary

Status: proposed

RedDB operational directory storage defines a local/primary-replica backup and
restore contract around consistent checkpoints, manifest validation, and retained
WAL. Coordinated cluster-wide backup is intentionally outside this first
contract.

## Decisions

**Backups start from a consistent checkpoint.** The initial operational backup
contract creates a checkpoint, copies the manifest plus data/index/segment files
covered by that checkpoint, and retains or copies WAL from that checkpoint
boundary forward.

**Restore replays WAL to a target LSN.** Restoring a backup loads the checkpoint,
validates the manifest and file checksums, then replays retained WAL up to the
requested target LSN before opening the store.

**WAL retention protects both PITR and replicas.** The WAL pruning boundary is
the maximum safety requirement across backup/PITR retention and the slowest
replica's durable restart LSN. Pruning must not break point-in-time recovery or
force avoidable replica rebootstrap.

**The first contract is local/primary-replica only.** Cluster-wide consistent
backup requires a separate distributed protocol over range ownership, watermarks,
and Supervisor coordination. This ADR does not define that protocol.

## Considered Options

- **Checkpoint plus WAL replay.** Chosen because it supports point-in-time
  recovery, validates the physical store, and composes naturally with
  primary-replica WAL streaming.
- **Checkpoint-only backup.** Rejected because it loses PITR and creates a weaker
  restore boundary.
- **Filesystem snapshot only.** Rejected as the only contract because RedDB still
  needs a database-level boundary independent of EBS/ZFS/LVM availability.
- **Logical-only restore.** Rejected for the initial operational backup path
  because it does not validate or restore physical layout, manifests, or segment
  files.
- **Cluster-wide backup now.** Deferred because it is a distributed protocol, not
  a local storage-layout property.

## Consequences

- Backup metadata must record checkpoint generation, manifest checksum, file
  checksums, WAL start LSN, and optional target LSN.
- WAL pruning must consult both backup retention and replica durable progress.
- Restore tooling must fail closed on manifest/checksum mismatch before replay.
- Cluster backup work needs a future ADR rather than being inferred from local
  backup behavior.
