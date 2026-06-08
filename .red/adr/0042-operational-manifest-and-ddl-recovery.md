# Operational Manifest and DDL Recovery

Status: proposed

RedDB operational directory storage uses an authoritative manifest to map logical
objects to physical files and defines crash-safe create/drop/recovery flows around
that manifest.

## Decisions

**The operational manifest is authoritative.** `red.manifest` maps collection,
index, range, and append-only segment identities to physical files and records
the checkpoint boundary needed for backup, recovery, repair, and range movement.
Naming conventions alone are not authoritative.

**Physical files are identified by stable IDs, not human names.** Human
collection/index names live in manifest/catalog metadata. Physical collection,
index, range, and segment files use stable internal identifiers so renames do not
move files and filesystem naming edge cases do not affect correctness.

**Logical IDs and physical file IDs are distinct.** Logical collection/range/index
IDs may be compact and sequential for catalog and WAL efficiency. Physical
`file_id`s use globally unique identifiers to avoid collisions across restore,
import/export, profile migration, and orphan quarantine.

**Manifest updates use copy-on-write atomic replace.** The first manifest update
strategy writes the next generation with checksum, fsyncs it, atomically renames
it into place, and fsyncs the containing directory.

**Create publishes physical files after they exist durably.** Creating a
collection or index first creates and fsyncs the physical file, then publishes it
in the manifest. If a crash happens before publication, the file is an orphan
candidate.

**Drop is two-phase through `drop_pending`.** Dropping a collection or index first
marks the manifest entry `drop_pending`, publishes that manifest generation,
deletes or quarantines the physical file, and then publishes a final manifest
generation that removes the entry.

**Orphan recovery quarantines by default.** Files found in the operational
directory but absent from the manifest are moved to `lost+found/` with recovery
metadata. RedDB does not delete or reattach such files automatically.

## Considered Options

- **Authoritative copy-on-write manifest.** Chosen because it is simple,
  checkable, and enough for a relatively small manifest in the first operational
  layout.
- **Naming convention discovery.** Rejected because it cannot safely distinguish
  live files, interrupted creates, interrupted drops, backup races, and corruption.
- **Human-readable names as physical file names.** Rejected because collection
  rename, tenant/schema collisions, filesystem encoding, and case sensitivity
  should not affect storage correctness.
- **Manifest log or multi-copy superblock immediately.** Deferred because they add
  complexity best justified if the manifest becomes large or hot.
- **Create by manifest-first.** Rejected because an authoritative manifest
  pointing to a missing file is harder and riskier to recover than a durable
  unreferenced file.
- **Drop by delete-first.** Rejected because it can leave the manifest pointing to
  missing data.
- **Automatic orphan attach/delete.** Rejected because it can either lose bytes or
  reintroduce data that was intentionally dropped.

## Consequences

- Operational recovery must scan manifest entries and physical files together.
- Backup tooling must treat `drop_pending` entries and `lost+found/` explicitly.
- Manifest generations need checksums and versioning from the first cut.
- Manifest entries must record both logical object identity and physical file
  identity.
- Future repair tooling can add administrative orphan inspection or restore
  commands without changing the default safe behavior.
