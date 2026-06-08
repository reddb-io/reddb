# Cluster Range File Layout

Status: proposed

RedDB multi-writer clusters organize local physical storage by shard/range, not by
whole collection, so ownership, repair, movement, and recovery share the same
operational unit.

## Decisions

**Cluster storage is physically range-oriented.** Each data member stores the
ranges it owns or replicates in a range file layout rather than a whole-collection
file layout. A small collection may still have one range, but the physical unit is
the range.

**Each range is a directory.** A range directory contains separate data, index, and
append-only segment files as needed. This keeps range movement operationally
bounded while preserving file-level separation for indexes and segment-based
models.

**A cluster node still uses one physical WAL per store.** WAL records carry range,
collection, index, transaction, term/epoch, and LSN identity. RedDB does not split
the physical WAL per range in the first cluster storage design.

**Move-range copies a physical snapshot before logical catch-up.** During a range
move, the target first copies a checkpoint/snapshot of the source range directory,
then catches up through the logical range-indexed stream. Only after catch-up does
the ownership catalog advance the epoch and move write authority to the target.

**Initial range repair uses full rebootstrap.** When a range replica is corrupt or
too stale to repair cheaply, the first implementation cut quarantines the local
range copy and rebootstrap it from a healthy owner using the same physical range
snapshot plus logical catch-up shape. Future repair may replace individual blocks
or segments by checksum when the manifest/checksum machinery is mature enough.

## Considered Options

- **Range directories.** Chosen because range ownership is the unit of write
  authority, failover, movement, and repair.
- **Whole-collection files in cluster storage.** Rejected because one collection
  may span multiple owners and moving a range should not require moving unrelated
  ranges from the same collection.
- **One monolithic file per range.** Rejected because it loses useful separation
  between data, indexes, and immutable append-only segments.
- **WAL per range.** Rejected because it sacrifices the simple sequential append
  path and complicates cross-range ordering; range filtering belongs in WAL
  records and logical stream indexes.
- **Logical rebuild from scratch for range movement.** Rejected because it is too
  slow for large ranges compared with snapshot plus delta catch-up.
- **Fine-grained block/segment repair in the first cut.** Deferred because full
  range rebootstrap delivers correct recovery with less implementation surface.

## Consequences

- Cluster backup/repair/movement tooling must understand range directories.
- The operational manifest and shard ownership catalog must connect logical
  range identity to local physical range directories.
- WAL records need enough identity to drive range-indexed replay and catch-up.
- Collection-level queries in cluster mode must resolve through ownership/range
  metadata rather than assuming one physical collection file.
- Repair tooling needs a quarantine path for broken local range directories and a
  rebootstrap path from a healthy owner.
