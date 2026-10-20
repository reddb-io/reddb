# Storage reliability comparison

Assessment date: 2026-10-19. Scope: page integrity, index consistency, crash
recovery, bounded storage growth, and operational verification. This is a source
and runtime qualification review, not a throughput benchmark or a certification.
Competitor behavior below assumes durable configurations and storage that honors
flush requests. No engine can make arbitrary failing hardware infallible.

## Assessment

RedDB has relevant mechanisms, but the evidence does not yet establish reliability
parity with MariaDB/InnoDB, PostgreSQL, or MongoDB/WiredTiger. The actual staging
workload failed qualification after the 1.24.9 release. The official 1.24.10
workflow binary subsequently passed the snapshot-based local qualification below;
deployed staging qualification remains a separate acceptance gate.

The checkpoint retirement design is conceptually closest to WiredTiger's delayed
block reclamation: publish a durable replacement before reusing a retired
generation, and retain generations still needed by readers. This similarity is
an architectural comparison, not proof of equivalent implementation quality.

## Local qualification of 1.24.10

Source: `8a8749872e1ba3e653c80b4c00d2731657b0732f`. The official Linux x86_64
workflow artifact was tested against a preserved copy of staging data, with
swap disabled and a one-CPU quota. These are local results, not deployed results
or competitor benchmarks.

- Full catalog inspection passed in 1,258 ms with a 3 GiB cgroup limit and no
  OOM events.
- Concurrent reads, writes, login, rollback, DDL, worker-style updates, readiness
  checks and three checkpoints passed with a 4 GiB cgroup limit. Checkpoints took
  512, 577 and 554 ms; the file did not grow.
- Twenty-four fixed-dataset update/checkpoint cycles did not grow the file.
  After forced process death, reopen preserved the acknowledged WAL write and
  did not expose the rolled-back row. Original business counts were unchanged.
- An independent read-only scan after those tests found no structural, value
  decoding or leaf-link faults across 131 collections. Hashes of 123 original
  collections were unchanged; five original technical collections changed as
  expected. This diagnostic scan is not a supported full row/index verifier.

These results cover specific reproduced failures. They do not establish
power-loss equivalence, comprehensive secondary-index correctness, bounded
storage for every workload, or safe general physical compaction.

## Mechanism comparison

| Concern | MariaDB / InnoDB | PostgreSQL | MongoDB / WiredTiger | RedDB evidence |
| --- | --- | --- | --- | --- |
| Partial page writes | Doublewrite buffer preserves a recoverable page before its destination is overwritten. | WAL full-page images protect page recovery. | New checkpoint publication preserves the previous valid checkpoint; journal recovery covers intervening changes. | Checksummed pages, in-file DWB, WAL, and dual superblocks exist. Recent allocator ownership fixes address a concrete page-reuse fault; exhaustive fault equivalence is not established. |
| Logical index correctness | `CHECK TABLE ... EXTENDED` checks row/index links in supported versions. | `amcheck` checks B-tree invariants and optionally heap coverage and uniqueness. | `validate` reports missing/extra index entries and invalid documents. | Index checkpoint/restart/WAL regressions exist. Catalog declaration/readiness comparisons are not a complete row-to-index validator. |
| Stable-size update workload | Purge and page reuse allow obsolete space to be reused, subject to transaction/history retention. | `VACUUM` reclaims dead versions for reuse, subject to visibility horizons. | Checkpoint block-manager extent lists delay reuse until old checkpoints are retired. | New retired-tree tracking and freelist fencing pass fixed-workload tests, including restart and crash cases. Historical orphan-page recovery is outside that fix. |
| Returning bytes to the filesystem | Tablespace and version dependent: table rebuild/optimization and newer optional system-tablespace autoshrink have different contracts. | Ordinary `VACUUM` primarily enables reuse; `VACUUM FULL` rewrites the table and requires extra space and locking. | `compact` attempts to return obsolete blocks; effectiveness depends on block location and workload. | `VACUUM FULL` currently forces persistence after MVCC vacuum; it is not a general paged-file rewrite/truncation operation. |
| Operator verification | `CHECK TABLE`, `innochecksum`, recovery modes, backup/restore procedures. | `amcheck`, checksums, `REINDEX`, WAL recovery and backup/restore procedures. | `validate`, journal/checkpoints, `compact`, replica resync and backup/restore procedures. | `SCRUB` and salvage APIs exist, but the inspected implementations target `RDBSBLK1` embedded artifacts rather than the current paged `RDDB` store. Their presence does not prove coverage of the staging format. |

## Concrete RedDB gaps

1. **Deployed staging qualification is still required.** The deployed 1.24.9 checkpoint
   returned HTTP 503 after 30,004 ms. Concurrent reads/writes remained responsive,
   but login and rollback timed out. A later catalog request OOM-killed the pod.
   The first follow-up optimized candidate also hit `MemoryMax=3G` in the actual
   snapshot reproducer. Version 1.24.10 removes full artifact reconstruction from
   the health path and passes that local reproducer without increasing its
   3 GiB limit. Increasing a limit alone would not establish boundedness.
2. **Inspection can trigger maintenance.** `RedDB::physical_metadata()` currently
   calls `load_or_bootstrap_physical_metadata(!read_only)`. When native metadata
   is ahead or its sidecar is missing, this can persist metadata and native
   artifacts from a nominally observational path. Artifact construction copies
   collections and builds search structures. The reproduced catalog OOM was
   addressed by removing payload reconstruction/decoding from health; this
   conditional getter publication remains a separate maintenance-boundary concern.
3. **Current-format verification needs explicit qualification.** The SQL `SCRUB`
   route calls `reddb_file::scrub_embedded_store`. That implementation reads two
   4 KiB `RDBSBLK1` authorities, checks an embedded snapshot, and checks a WAL
   boundary. It does not walk all current paged B-trees, verify allocator
   disjointness, or compare all secondary entries against visible records.
   Embedded salvage similarly operates on snapshot/WAL artifacts. Do not run it
   as an assumed paged-store repair procedure.
4. **Reclamation is not the same as shrinking.** Retiring old checkpoint trees
   prevents one source of future growth. It does not find every pre-upgrade
   orphan page, remove intentionally retained history, or shrink the existing
   file. The present `VACUUM FULL` name must not be interpreted as PostgreSQL's
   physical rewrite contract.
5. **Failure handling needs operational proof.** A process kill tests a different
   failure boundary from power loss, partial writes, ENOSPC, or failed fsync.
   Existing tests are valuable but do not replace campaigns at each publication
   boundary and restore drills on the deployed storage path.

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

1. Qualify the real staging snapshot with the intended memory budget and
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
