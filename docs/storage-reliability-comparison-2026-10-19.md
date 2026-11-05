# Storage reliability comparison

Assessment updated: 2026-10-20. Scope: page integrity, index consistency, crash
recovery, bounded storage growth, and operational verification. This is a source
review and a set of qualification criteria, not a throughput benchmark or a
certification. Competitor behavior assumes durable configurations and storage
that honors flush requests. No engine makes arbitrary failing hardware infallible.

## Assessment

RedDB has relevant mechanisms, but targeted regression tests do not establish
reliability parity with MariaDB/InnoDB, PostgreSQL, or MongoDB/WiredTiger.
Qualification must cover the actual persistent format, durability settings,
resource budget, storage latency, concurrency and failure boundary.

Recent fixes fence WAL reset through durable acknowledgement, preserve collection
definitions when embedded options open a paged file, avoid redundant catalog
health observations, and reject main-file-only backups that omit required paged
metadata. Their regression tests prove specific reproduced failures. They do not
prove universal corruption prevention, bounded growth under every workload, or
safe general physical compaction. Deployment-specific evidence belongs in the
operator's private acceptance record.

The checkpoint retirement design is conceptually closest to WiredTiger's delayed
block reclamation: publish a durable replacement before reusing a retired
generation, and retain generations still needed by readers. Architectural
similarity is not proof of equivalent implementation quality.

## Mechanism comparison

| Concern | MariaDB / InnoDB | PostgreSQL | MongoDB / WiredTiger | RedDB evidence |
| --- | --- | --- | --- | --- |
| Partial page writes | Doublewrite buffer preserves a recoverable page before its destination is overwritten. | WAL full-page images protect page recovery. | New checkpoint publication preserves the previous valid checkpoint; journal recovery covers intervening changes. | Checksummed pages, in-file DWB, WAL, and dual superblocks exist. Recent allocator ownership fixes address a concrete page-reuse fault; exhaustive fault equivalence is not established. |
| Logical index correctness | `CHECK TABLE ... EXTENDED` checks row/index links in supported versions. | `amcheck` checks B-tree invariants and optionally heap coverage and uniqueness. | `validate` reports missing/extra index entries and invalid documents. | Index checkpoint/restart/WAL regressions exist. Catalog declaration/readiness comparisons are not a complete row-to-index validator. |
| Stable-size update workload | Background purge and page reuse allow obsolete space to be reused, subject to transaction/history retention. | Autovacuum and manual `VACUUM` reclaim dead versions for reuse, subject to visibility horizons. | Checkpoint block-manager extent lists delay reuse until old checkpoints are retired. | Retired-tree tracking and freelist fencing pass specific fixed-workload tests. MVCC vacuum is currently manual (ADR 0014); historical orphan-page recovery and automatic MVCC collection remain gaps. |
| Returning bytes to the filesystem | Tablespace and version dependent: table rebuild/optimization and newer optional system-tablespace autoshrink have different contracts. | Ordinary `VACUUM` primarily enables reuse; `VACUUM FULL` rewrites the table and requires extra space and locking. | `compact` attempts to return obsolete blocks; effectiveness depends on block location and workload. | `VACUUM FULL` currently forces persistence after MVCC vacuum; it is not a general paged-file rewrite/truncation operation. |
| Operator verification | `CHECK TABLE`, `innochecksum`, recovery modes, backup/restore procedures. | `amcheck`, checksums, `REINDEX`, WAL recovery and backup/restore procedures. | `validate`, journal/checkpoints, `compact`, replica resync and backup/restore procedures. | Embedded `SCRUB`/salvage explicitly reject the current paged format in 1.24.11. A complete supported current-format verifier and repair path are still required. |

## Concrete RedDB gaps

1. **Storage-path qualification is required.** Successful CI, a local test on fast
   storage or Kubernetes readiness does not prove latency or memory boundedness
   on a different storage path. Exercise concurrent reads, writes, login,
   rollback, catalog inspection and checkpoints under the intended limits.
2. **WAL reset must preserve acknowledgement ordering.** Qualify reset, flusher
   publication and acknowledged-write recovery together. A successful standalone
   checkpoint does not cover concurrent rollback/update failures.
3. **Current-format verification needs explicit qualification.** The SQL SCRUB
   route calls reddb_file::scrub_embedded_store. Its embedded authority,
   snapshot and WAL checks do not walk all current paged B-trees, verify
   allocator disjointness, or compare every secondary entry against visible
   records. Embedded salvage is not a general paged-store repair procedure.
4. **Reclamation is not shrinking.** Retiring old checkpoint trees prevents one
   source of growth. It does not find every historical orphan page, remove
   intentionally retained history or shrink the existing file. Ordinary MVCC
   vacuum currently requires an explicit maintenance operation. VACUUM FULL
   must not be interpreted as PostgreSQL's physical rewrite contract.
5. **Failure handling needs operational proof.** Process death differs from power
   loss, partial writes, ENOSPC and failed fsync. Inject faults at each
   publication boundary and restore a complete backup independently. A paged
   store opened through embedded options still needs its required sidecars.

## Source pointers

- `crates/reddb-server/src/storage/engine/pager.rs`: checksum defaults, dual
  authorities and in-file DWB layout.
- `crates/reddb-server/src/storage/unified/store/impl_pages.rs`: tree retirement,
  streamed checkpoint construction, and payload/catalog decode failures.
- `crates/reddb-server/tests/paged_checkpoint_rebuild.rs`: changed/unchanged
  checkpoints, retirement across restart, cursor pins, allocator crash cases,
  document-index recovery, malformed payloads and idempotent index creation.
- `crates/reddb-server/src/storage/unified/devx/reddb/impl_registry.rs`:
  `physical_metadata()` observation path.
- `crates/reddb-server/src/storage/unified/devx/reddb/impl_metadata.rs`:
  conditional metadata bootstrap/publication and native artifact construction.
- `crates/reddb-server/src/storage/unified/devx/reddb/impl_access.rs`:
  `native_vector_artifact_records()` and collection-wide copies.
- `crates/reddb-file/src/scrub.rs`, `crates/reddb-file/src/salvage.rs`, and
  `crates/reddb-server/src/runtime/impl_scrub.rs`: actual verifier/repair scope.
- `crates/reddb-server/src/runtime/impl_core.rs`: MVCC vacuum and FULL persistence.

## Acceptance gates

These are proposed release criteria, not completed results:

1. Qualify a representative complete fixture with the intended memory budget and
   concurrent reads, writes, login, rollback, catalog inspection and checkpoints.
   Catalog inspection must not publish storage artifacts or copy whole payload
   collections. Stop at the first failed checkpoint.
2. Run a shared acknowledged-write oracle with fixed-size updates, deletes,
   reinserts, secondary equality/range queries and rolling checkpoints. Compare
   each recovered result set with the oracle, not just row counts. Measure
   logical data, file allocation, WAL/history, reusable bytes and peak scratch
   space separately. Require steady-state storage to plateau under a defined
   retention horizon; retained snapshots may legitimately prevent reuse.
3. Inject process death, partial page writes, failed sync and ENOSPC at allocator
   and publication boundaries. Preserve acknowledged durable commits; reject
   corruption explicitly where recovery is impossible. Never silently skip a
   malformed row or continue with a partial catalog.
4. Provide and test a bounded current-format integrity walker: page checksums,
   tree ordering/links, live/retired/free page disjointness, document decoding,
   and both directions of row/index coverage. Separate detection from repair.
5. Qualify safe index rebuild and compaction/rewrite on a copy, with interrupted
   maintenance and insufficient-space tests. Report retained history and any
   unrepairable region; preserve the original artifact and backups.
6. Exercise the same logical workload on pinned competitor versions with their
   durable settings. Publish hardware, filesystem, resource budgets, driver
   versions, compression, retention, concurrency, durability acknowledgements
   and raw results. Until then, make no relative throughput/latency or storage
   efficiency claim.

## Official references

- [MariaDB InnoDB doublewrite buffer](https://mariadb.com/docs/server/server-usage/storage-engines/innodb/innodb-doublewrite-buffer)
- [MariaDB CHECK TABLE](https://mariadb.com/docs/server/reference/sql-statements/table-statements/check-table)
- [MariaDB innochecksum](https://mariadb.com/docs/server/clients-and-utilities/administrative-tools/innochecksum)
- [MariaDB InnoDB system tablespaces and autoshrink](https://mariadb.com/docs/server/server-usage/storage-engines/innodb/innodb-tablespaces/innodb-system-tablespaces)
- [PostgreSQL reliability and full-page writes](https://www.postgresql.org/docs/current/wal-reliability.html)
- [PostgreSQL routine vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html)
- [PostgreSQL amcheck](https://www.postgresql.org/docs/current/amcheck.html)
- [PostgreSQL REINDEX](https://www.postgresql.org/docs/current/sql-reindex.html)
- [MongoDB WiredTiger checkpoints and journal](https://www.mongodb.com/docs/manual/core/wiredtiger/)
- [MongoDB validate](https://www.mongodb.com/docs/manual/reference/command/validate/)
- [MongoDB compact](https://www.mongodb.com/docs/manual/reference/command/compact/)
- [WiredTiger 11.2 block-manager architecture](https://source.wiredtiger.com/11.2.0/arch-block.html)

Documentation describes mechanisms and configuration-dependent behavior. It is
not evidence that competitor databases cannot corrupt, nor that RedDB matches
them merely by implementing similarly named mechanisms.
