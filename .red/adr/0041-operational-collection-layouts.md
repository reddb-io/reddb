# Operational Collection Layouts

Status: proposed

RedDB operational directory storage uses different physical layouts for mutable
collections and append-only collections while keeping a single physical WAL per
node/store and deriving logical replication streams for replicas and clusters.

## Decisions

**Mutable collections use stable object files.** Ordinary mutable collections and
indexes are stored in stable physical files such as `collection_id.rdb` and
`collection_index_id.rdb`, following a WiredTiger/Postgres-like separation of
logical objects into independently managed files.

**Append-only collections use immutable segments.** Append-only tables,
timeseries, event-shaped data, and similar models use immutable closed segments
plus compaction and retention rather than in-place mutable pages. Native
append-shaped models may infer this layout; ordinary tables must declare
append-only intent explicitly.

**Closed append-only segments are immutable.** Compaction and retention create
replacement segments and retire old segments rather than patching closed segments
in place.

**The initial append-only contract is strict.** Append-only collections do not
support logical `UPDATE` or `DELETE` in the first contract. Data retirement
happens through retention, TTL, or compaction policy rather than tombstones.

**Append-only segments carry lookup metadata.** Each closed segment contains data,
primary index, min/max statistics, checksum coverage, manifest metadata, and
optional lookup accelerators such as bloom filters or summaries.

**Append-only segment chunks are 512 KiB.** Segment indexes/manifests point to
fixed-size 512 KiB chunks and store each chunk's expected checksum. The fixed
chunk size supports predictable prefetch, validation, multipart copy, and future
fine-grained repair.

**Compression starts at append-only segment granularity.** Immutable append-only
segments may be compressed independently. The initial default codec is zstd, with
`none` available for already-compressed or CPU-bound workloads. Mutable collection
files do not use page/block compression in the first design.

**Checksums cover mutable pages and immutable segment blocks.** Mutable collection
pages carry checksums in their page headers. Append-only segment blocks/chunks are
verified against checksums stored in segment index/manifest metadata. Manifests
and checkpoint metadata are checksummed as well.

**Operational storage uses one physical WAL per node/store.** WAL records carry
collection, index, range, transaction, and LSN identity rather than splitting WAL
files per collection or index. This preserves a sequential append path and
cross-collection transaction recovery.

**Replication uses a derived logical stream.** The physical WAL is for local crash
recovery. Replicas, range movement, bootstrap, and repair use a derived logical
replication stream that can be filtered by collection/range and versioned
separately from physical page layout.

## Considered Options

- **Mutable files plus append-only immutable segments.** Chosen because mutable
  OLTP-style data and append-shaped timeseries/event data have different storage
  economics.
- **One layout for every collection.** Rejected because in-place pages are a poor
  fit for high-volume append/retention workloads, while immutable segments add
  unnecessary read/compaction complexity to ordinary mutable collections.
- **One WAL per collection/index.** Rejected because it complicates
  cross-collection transaction atomicity and sacrifices the simple sequential WAL
  write path.
- **Physical WAL as the replication protocol.** Rejected as the primary contract
  because replicas and cluster range movement need semantic filtering,
  range-indexed recovery, and physical-layout evolution.
- **LSM-style tombstones immediately.** Rejected for the first append-only
  contract because tombstones make compaction, reads, and semantics substantially
  more complex.
- **Mutable page/block compression immediately.** Deferred because it complicates
  the write path, WAL/recovery, and page cache more than segment compression.
- **WAL/manifest-only checksums.** Rejected because corruption in individual data
  pages or segment blocks must be detectable without relying only on recovery
  logs.

## Consequences

- Collection creation must choose a storage layout explicitly or infer it from a
  native append-shaped model such as timeseries.
- WAL records need stable identity fields for collection, index, range,
  transaction, and LSN.
- Backup, repair, and replication tooling must understand both mutable object
  files and append-only segment manifests.
- Segment manifests must record compression codec and any metadata required to
  validate and decode compressed segments.
- Validation and future repair tooling can rely on page and segment-block
  checksum boundaries.
- Future support for logical delete/update on append-only collections would be a
  separate LSM/tombstone design, not an implicit extension of the first contract.
