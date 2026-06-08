# Operational Storage Profiles

Status: proposed

RedDB distinguishes lightweight single-file storage from operational storage
layouts used for primary-replica production deployments and multi-writer clusters.

## Decisions

**Primary-replica supports two local packaging modes.** Small/dev
primary-replica deployments may use one zoned `.rdb` file per node. Production
presets require an operational directory layout when replicas, managed backup, or
WAL retention are enabled.

**Preset selection is guided, not automatic migration.** RedDB should not silently
promote a running single-file deployment into an operational directory layout.
Dev/small presets default to single-file; production presets make operational
layout required once HA or backup intent is declared.

**Cluster storage is always operational directory layout.** Multi-writer cluster
nodes do not use embedded single-file packaging. They need explicit local
structure for range movement, snapshots, repair, WAL retention, range-indexed
recovery, and operator diagnostics.

**Logical records stay shared across profiles.** Storage/deploy profiles may use
different physical packaging, but they should share core logical concepts such as
collection identity, range identity, LSN/term, ownership epoch, checksums, and
manifest/checkpoint boundaries where applicable.

## Considered Options

- **Guided primary-replica split plus operational-only cluster.** Chosen because
  it preserves local simplicity while making production HA and cluster behavior
  explicit.
- **One physical layout for every profile.** Rejected because embedded
  portability, serverless hot boot, primary-replica backup, and cluster range
  movement optimize for different operational constraints.
- **Automatic single-file to operational promotion.** Rejected because a surprise
  physical migration during config changes can create unclear backup, rollback,
  and recovery semantics.
- **Single-file cluster nodes.** Rejected because the ergonomics gained are small
  compared with the cost to range streaming, repair, retention, and diagnostics.

## Consequences

- Configuration and presets must make storage/deploy profile selection visible.
- Backup, restore, and migration tooling must understand both single-file and
  operational directory layouts.
- Cluster implementation can assume directory-level operational metadata instead
  of preserving embedded single-file constraints.
- Documentation must describe conversion paths explicitly rather than implying
  one universal physical format.
