# Embedded Single-file Zoned RDB

Status: proposed

RedDB's embedded storage profile uses a single `.rdb` file as the operator-visible
durable artifact. The file is internally zoned so it can carry all required
storage-engine state without mandatory sidecars.

## Decisions

**Embedded RedDB is single-file by contract.** Embedded, local, test, plugin, and
prototype use cases must be able to create, copy, move, back up, and delete one
`.rdb` file containing all required durable state.

**The single file is zoned internally.** The target `.rdb` layout has explicit
zones for superblock copies, manifest/catalog state, WAL records, page/grid
storage, free-space metadata, checksums, and future overflow/blob extents. The
file should feel SQLite-like to users while borrowing TigerBeetle's internal
discipline around superblocks, manifests, checksummed block references, and
replayable state.

**The embedded manifest is internal and authoritative.** The `.rdb` contains an
internal manifest rooted by the file superblock. It maps internal zones and
logical objects such as collections, indexes, WAL region, free-space state, and
checkpoint boundary without requiring an external `red.manifest`.

**The embedded WAL lives inside the file.** Embedded single-file storage uses a
circular internal WAL region. WAL entries may be overwritten only after the
checkpoint/superblock boundary proves they are no longer needed for recovery.

**The embedded superblock uses two ping-pong copies.** The first target design
uses a pair of superblock copies with generation and checksum metadata. Open
chooses the newest valid copy to root the embedded internal manifest.

**Embedded storage is checksummed at its recovery boundaries.** Mutable pages
carry checksums in their page headers. Immutable internal blocks or segment-like
regions are verified against checksums stored in manifest/index metadata.
Superblocks, embedded manifest state, and checkpoint metadata are checksummed.

**Sidecars are not the embedded target contract.** Existing sidecars such as WAL,
metadata, and double-write files may remain as legacy or transitional
implementation details while the zoned format is introduced, but the promoted
embedded profile must not require them for normal operation.

**This does not constrain every deployment profile.** Serverless, primary-replica,
and cluster profiles may choose different physical packaging when boot speed,
replication streaming, snapshot distribution, range movement, or cluster repair
make a directory or segmented layout more appropriate.

## Considered Options

- **Single zoned `.rdb`.** Chosen because it preserves the embedded/SQLite-like
  user experience while giving the engine room for formal recovery, checksums,
  checkpointing, and future format evolution.
- **External manifest or WAL sidecars.** Rejected for the promoted embedded
  contract because the `.rdb` must remain self-contained.
- **Four superblock copies.** Deferred because a two-copy ping-pong design is a
  smaller first target while still avoiding a single fixed root copy.
- **Manifest-only checksums.** Rejected because localized page/block corruption
  must be detectable during reads, backup validation, and future repair.
- **Primary `.rdb` plus mandatory sidecars.** Rejected as the embedded target
  because it weakens copy/delete/backup ergonomics and makes local/plugin usage
  easier to corrupt operationally.
- **Current layout only.** Rejected as the target because it does not encode the
  storage/deploy profile distinction and keeps embedded ergonomics tied to
  transitional pager implementation details.

## Consequences

- The embedded storage roadmap needs a real file-level manifest/superblock model
  rather than relying only on external path conventions.
- WAL, metadata, DWB, and future recovery state need an in-file home before the
  embedded profile can be considered promoted.
- Embedded open/recovery must validate superblock generations/checksums, load the
  internal manifest, and replay the internal WAL region from the checkpoint
  boundary.
- Read and validation paths must check mutable page checksums and immutable
  block/segment checksums against their expected metadata.
- Migration must distinguish legacy sidecar-backed databases from zoned embedded
  `.rdb` databases.
- Cluster and replication work must not assume the embedded single-file packaging
  is the only valid physical store layout.
