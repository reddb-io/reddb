# Storage reliability comparison

Assessment updated: 2026-10-20. Scope: page integrity, index consistency, crash
recovery, bounded storage growth, and operational verification. This is a source
and runtime qualification review, not a throughput benchmark or a certification.
Competitor behavior below assumes durable configurations and storage that honors
flush requests. No engine can make arbitrary failing hardware infallible.

## Assessment

RedDB has relevant mechanisms, but the evidence does not yet establish reliability
parity with MariaDB/InnoDB, PostgreSQL, or MongoDB/WiredTiger. The actual staging
workload failed qualification after the 1.24.9 release. Official 1.24.10 and
1.24.11 artifacts have since passed specific local checks, but concurrent
rollback/update qualification exposed a separate WAL acknowledgement/reset race.
The WAL epoch fix is merged, but the official 1.24.12 artifact exposed a further
legacy paged-store collection-definition loss during expanded recovery testing.
Successful CI, publication and Kubernetes readiness do not close that runtime
acceptance gate.

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
| Stable-size update workload | Background purge and page reuse allow obsolete space to be reused, subject to transaction/history retention. | Autovacuum and manual `VACUUM` reclaim dead versions for reuse, subject to visibility horizons. | Checkpoint block-manager extent lists delay reuse until old checkpoints are retired. | Retired-tree tracking and freelist fencing pass specific fixed-workload tests. MVCC vacuum is currently manual (ADR 0014); historical orphan-page recovery and automatic MVCC collection remain gaps. |
| Returning bytes to the filesystem | Tablespace and version dependent: table rebuild/optimization and newer optional system-tablespace autoshrink have different contracts. | Ordinary `VACUUM` primarily enables reuse; `VACUUM FULL` rewrites the table and requires extra space and locking. | `compact` attempts to return obsolete blocks; effectiveness depends on block location and workload. | `VACUUM FULL` currently forces persistence after MVCC vacuum; it is not a general paged-file rewrite/truncation operation. |
| Operator verification | `CHECK TABLE`, `innochecksum`, recovery modes, backup/restore procedures. | `amcheck`, checksums, `REINDEX`, WAL recovery and backup/restore procedures. | `validate`, journal/checkpoints, `compact`, replica resync and backup/restore procedures. | Embedded `SCRUB`/salvage explicitly reject the current paged format in 1.24.11. A complete supported current-format verifier and repair path are still required. |

## Additional qualification, 2026-10-20

The signed 1.24.10 image restored staging Auth with a 1 GiB engine budget inside
its unchanged 2 GiB container limit. An isolated copy passed writes, rollback,
three checkpoints in 90/150/159 ms and acknowledged-write survival across pod
replacement. After reconnecting the real application, a checkpoint took 26,404
ms. The platform store's live checkpoint returned HTTP 503 after 30,016 ms;
concurrent reads/writes remained fast while login/catalog reached their deadlines.
No checkpoint retries were started. A timed-out caller does not cancel engine work.

The official 1.24.11 Linux artifact passed full-catalog inspection in 1,308 ms
under a 3 GiB cgroup limit without OOM. Three local checkpoints took
567/620/512 ms, but concurrent native updates of `worker_leases` timed out.
Separate connections did not remove that failure. A reduced update/checkpoint
workload passed; adding concurrent rollback reproduced the stall.

A coordinator regression pauses a writer after enqueue, before acknowledgement.
The published implementation allows checkpoint reset to discard that queued
record and invalidate its reserved offset. The proposed shared WAL-epoch fence
keeps append leases through acknowledgement, excludes stale flusher publication,
and lets the flusher drain existing leases while reset waits. That regression
fails before the patch and passes afterward; complete snapshot/runtime and
deployed qualification remain required before calling the issue closed.

The merged WAL epoch candidate passed the previously failing concurrent real-copy
workload with checkpoints in 566/602/573 ms, no observed file growth, and no OOM.
Twenty-four update/checkpoint cycles also plateaued. Forced process termination
preserved both an acknowledged probe write and a native worker-lease update;
the rolled-back row remained invisible. This does not establish deployed
acceptance or remove the separate catalog latency limit: with positioned I/O
throttled to 12 MiB/s, catalog requests exceeded their ten-second deadline.

Expanded equality/range/update/delete/reinsert testing found another failure in
the real copy and reproduced it in a roughly 1.4 MiB paged database with the
official 1.24.12 Linux workflow binary. Opening an existing paged file with
embedded options incorrectly chose the binary dump's auxiliary contract blob.
Paged checkpoints do not persist that blob. After reopen, a newly declared
document collection lost its contract and unmarked UPDATE used table semantics;
the resulting visible document values differed from the acknowledged-write
oracle. A newly created embedded artifact did not reproduce that failure.
The regression creates a paged store, opens it with embedded options, creates a
document and index, checkpoints, reopens, and verifies the declared model and
document post-image. Contract creation/removal and boot cache seeding must use
the actual paged-file boundary. A first follow-up candidate additionally enabled
full metadata checkpoint publication for this compatibility path; creating a
document in the real copy then exceeded twenty seconds and approached the
4 GiB test limit, before any checkpoint cycle completed. Contract-only writes
must retain the canonical durable sidecar and publication lock without invoking
the unrelated whole-store search-artifact rebuild. General checkpoint/artifact
refresh behavior is preserved. The deployment remains gated on qualification
of this additional fix.

Manual vacuum was also qualified on a preserved, older Auth copy with the
official 1.24.11 binary, a 1 GiB engine budget and a 2 GiB cgroup limit. It
removed 675,058 dead versions from the four Auth tables, leaving 4,109 physical
versions matching the visible row counts. Counts and hashes of non-null encrypted
payloads were unchanged after checkpoint and cold reopen. Peak cgroup memory was
1,347,887,104 bytes. A bounded independent scan found no structure, value or
leaf-link faults. This scan is not a complete row/index/allocator verifier.
The file stayed at 366,084,096 bytes: vacuum made pages reusable; it did not
perform physical shrinking. This prior-copy result does not authorize treating
the current live store as already vacuumed or compacted.

## Concrete RedDB gaps

1. **Deployed staging qualification is still required.** The deployed 1.24.9 checkpoint
   returned HTTP 503 after 30,004 ms. Concurrent reads/writes remained responsive,
   but login and rollback timed out. A later catalog request OOM-killed the pod.
   The first follow-up optimized candidate also hit `MemoryMax=3G` in the actual
   snapshot reproducer. Version 1.24.10 removes full artifact reconstruction from
   the health path and passes that local reproducer without increasing its
   3 GiB limit. Increasing a limit alone would not establish boundedness.
2. **WAL reset must preserve acknowledgement ordering.** The concurrent
   rollback/update regression above is not covered by a healthy idle catalog or
   successful standalone checkpoints. Qualify reset, flusher publication and
   acknowledged-write recovery together. Version 1.24.11 already makes physical
   metadata/health inspection observational and drains periodic runtime workers
   before releasing storage; those fixes do not, by themselves, close this race.
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
