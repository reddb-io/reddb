# Operational Storage Profiles

> [!NOTE]
> This page describes a **target storage design**, not the current shipped
> on-disk format. The decisions below come from ADRs 0038–0045, all of which are
> currently **proposed**. What ships today is the single-file `.rdb` page format
> with sidecar shadows documented in
> [`.rdb` File Format](file-format.md); the zoned, directory, and segment-pack
> layouts here are the planned evolution, surfaced so deployment docs and
> operators can reason about where each profile is headed.

RedDB does not assume one physical packaging for every deployment. The target
model separates a small set of **storage profiles** so embedded portability,
serverless cold-boot, primary-replica backup, and cluster range movement can each
optimize for their own constraints, while sharing the same *logical* concepts —
collection identity, range identity, LSN/term, ownership epoch, checksums, and
manifest/checkpoint boundaries.

## Profile matrix

The profile distinction is the core decision in
[ADR 0040 — Operational Storage Profiles](../../.red/adr/0040-operational-storage-profiles.md).

| Profile | Packaging | Notes |
|---|---|---|
| Embedded | Single zoned `.rdb` file | SQLite-like one-file artifact; self-contained, no mandatory sidecars (target). ADR 0038. |
| Serverless | `.rdb` plus a derived segment pack | `.rdb` stays canonical; a manifest + immutable parts + delta WAL segments support object storage, multipart copy, and hot boot. ADR 0039. |
| Primary-replica | Single-file for dev/small; **operational directory** for production | Operational layout becomes required once replicas, managed backup, or WAL retention are enabled. ADR 0040/0044. |
| Cluster | **Operational directory**, range-oriented | Always directory layout; physical files are organized by shard/range. ADR 0040/0045. |

Two product rules follow from this matrix:

- **Preset selection is guided, not automatic migration.** RedDB does not
  silently promote a running single-file deployment into an operational directory
  layout. Dev/small presets default to single-file; production presets make the
  operational layout required once HA or backup intent is declared.
- **Cluster nodes never use embedded single-file packaging**, because range
  movement, snapshots, repair, WAL retention, and range-indexed recovery need
  explicit local structure.

## Embedded: single-file zoned `.rdb`

[ADR 0038](../../.red/adr/0038-embedded-single-file-zoned-rdb.md) targets a single
`.rdb` file that carries all required durable state, internally zoned for
superblock copies, manifest/catalog state, a circular WAL region, page/grid
storage, free-space metadata, and checksums. The design borrows TigerBeetle-style
discipline (ping-pong superblocks, checksummed block references, replayable
state) while keeping a SQLite-like single-file user experience.

Sidecars such as the double-write buffer, header/metadata shadows, and external
WAL remain transitional implementation details while the zoned format is
introduced; the promoted embedded profile must not require them. The sidecars
that ship today are documented in [`.rdb` File Format](file-format.md#companion-files).

## Serverless: derived segment pack

[ADR 0039](../../.red/adr/0039-serverless-rdb-segment-pack.md) keeps `.rdb` as the
one canonical database artifact and lets a runtime export it to — and hydrate it
from — a derived *segment pack*: a manifest plus immutable parts and delta WAL
segments. The pack exists to support object-storage caching, multipart copy, and
fast cold boot. It must round-trip: exporting `.rdb` to a pack and hydrating it
back must preserve the same logical state, checksums, and recovery boundary. It is
a serverless packaging optimization, never a second database format users have to
reason about separately, and it does not weaken embedded single-file semantics.

## Operational directory layout

Primary-replica production and all cluster deployments use an operational
directory rather than a single file. The directory model is split across several
ADRs:

### Forking an embedded single-file store

Store forks (ADR 0070) require the operational directory substrate because the
fork manifest shares immutable artifacts with the parent and hydrates mutable
collection files lazily on first write. An embedded single-file `.rdb` has no
shared-segment substrate, so RedDB rejects direct fork attempts on that profile
instead of silently copying the file.

The supported path is explicit:

1. Open the embedded `.rdb` source and create a named physical export.
2. Open the exported `.rdb` with an operational-directory storage profile, such
   as `primary-replica-production-ha`.
3. Fork that operational store. The fork records the exported store's identity
   as `parent_store` and the current durable LSN as `fork_lsn`.

With the runtime API, the shape is:

```rust
let export = single_file_runtime.create_export("fork-source")?;
let operational = RedDBRuntime::with_options(
    RedDBOptions::persistent(&export.data_path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())?,
)?;
let fork = operational.fork_store("experiment")?;
```

Calling `fork_store` on the original embedded single-file runtime returns a
didactic error that points back to this export path.

### Collection layouts (ADR 0041)

[ADR 0041](../../.red/adr/0041-operational-collection-layouts.md) gives mutable and
append-only collections different physical shapes:

- **Mutable collections** use stable object files such as `collection_id.rdb` and
  `collection_index_id.rdb` (WiredTiger/Postgres-style file separation).
- **Append-only collections** (timeseries, events, logs) use immutable closed
  segments plus compaction and retention rather than in-place pages. The first
  contract is strict: no logical `UPDATE`/`DELETE`; retirement happens through
  retention, TTL, or compaction. Closed segments are immutable.
- **Segment chunks are 512 KiB**, each with an expected checksum recorded in the
  segment index/manifest, supporting predictable prefetch, validation, multipart
  copy, and future fine-grained repair.
- **Compression starts at append-only segment granularity** — default `zstd`,
  with `none` available. Mutable collection files do not use page/block
  compression in the first design.
- **One physical WAL per node/store.** WAL records carry collection, index,
  range, transaction, and LSN identity; replicas and range movement consume a
  *derived logical stream*, not the physical WAL.

### Manifest and DDL recovery (ADR 0042)

[ADR 0042](../../.red/adr/0042-operational-manifest-and-ddl-recovery.md) makes
`red.manifest` the authoritative map from logical objects (collection, index,
range, segment) to physical files, plus the checkpoint boundary. Key rules:

- Physical files are identified by stable internal IDs (globally unique
  `file_id`s), not human names, so renames do not move files.
- Manifest updates are copy-on-write atomic replace: write the next generation
  with a checksum, fsync, atomically rename, fsync the directory.
- **Create publishes after durability** (file created and fsynced, then
  published); **drop is two-phase** through a `drop_pending` manifest state.
- **Orphan recovery quarantines by default**: files present on disk but absent
  from the manifest move to `lost+found/` with recovery metadata, never auto-
  deleted or auto-attached.

### Backup and restore boundary (ADR 0043)

[ADR 0043](../../.red/adr/0043-operational-backup-restore-boundary.md) defines a
local/primary-replica backup contract: a backup starts from a consistent
checkpoint, copies the manifest plus covered data/index/segment files, and retains
WAL from the checkpoint boundary forward. Restore loads the checkpoint, validates
the manifest and file checksums, then replays WAL to a target LSN before opening
the store. WAL pruning is bounded by the stricter of PITR retention and the
slowest replica's durable restart LSN. **Coordinated cluster-wide backup is
explicitly out of scope** for this first contract and needs a separate distributed
protocol.

## Cluster: range file layout

[ADR 0045](../../.red/adr/0045-cluster-range-file-layout.md) organizes cluster
local storage by shard/range so ownership, repair, movement, and recovery share
one operational unit:

- **Each range is a directory** containing separate data, index, and append-only
  segment files as needed.
- The node still keeps **one physical WAL per store**; WAL records carry range,
  collection, index, transaction, term/epoch, and LSN identity rather than being
  split per range.
- **Move-range copies a physical snapshot first, then catches up** through the
  logical range-indexed stream; only after catch-up does the ownership catalog
  advance the epoch and move write authority.
- **Initial range repair uses full rebootstrap** — a corrupt or too-stale range
  replica is quarantined locally and rebootstrapped from a healthy owner. Finer
  block/segment-level repair by checksum is deferred.

The cluster routing, ownership-catalog, and cross-range model that sits above this
layout is documented in [Cluster Sharding](../architecture/cluster-sharding.md).

## Shared logical concepts

Whatever the physical packaging, profiles share core logical identity so backup,
replication, and recovery tooling can reason across them:

- collection / range / index identity;
- LSN and term;
- ownership epoch (cluster);
- page and segment-block checksums;
- manifest / checkpoint boundaries (operational and embedded-internal).

This is why a logical replication stream — not the physical WAL — is the
replication and range-movement contract across primary-replica and cluster
profiles (ADR 0041/0044).

## See also

- [`.rdb` File Format](file-format.md) — the shipped single-file page format.
- [Cluster Sharding](../architecture/cluster-sharding.md) — range ownership,
  hash slots, and cross-range operations.
- [Distributed Roadmap](../architecture/distributed-roadmap.md) — what is landed
  versus planned across the distributed stack.
- [Replication](../deployment/replication.md) — primary-replica runtime today.
