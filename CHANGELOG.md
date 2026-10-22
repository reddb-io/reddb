# Changelog

## 1.25.4

### Patch Changes

- [#2448](https://github.com/reddb-io/reddb/pull/2448) [`322bfca`](https://github.com/reddb-io/reddb/commit/322bfcad9ba9f8ff17923787fc1dd7b2078caa32) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Checkpoint small WAL-backed table updates without rereading and replacing untouched payloads. Keep bounded mutation and WAL-image proofs, conservative fallbacks and the existing publication fences. Preserve the canonical inserted row image in versioned UPDATE WAL records, and keep runtime statistics available when embedded options open a native paged store.

## 1.25.3

## 1.25.2

### Patch Changes

- [#2443](https://github.com/reddb-io/reddb/pull/2443) [`c9a92f4`](https://github.com/reddb-io/reddb/commit/c9a92f4db670e4eeddaaf2165a6fc5ac63c97f18) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Defer reuse of physically removed B-tree and overflow pages until their removal
  from the collection catalog is durable. Preserve pending reclamation across
  reopen with an optional paged-metadata ledger, and publish physical delete
  batches without serializing unchanged survivors. Add crash/reuse, tree-shrink
  and repeated-reopen storage regressions. Older readers can ignore the new
  ledger; older writers do not preserve its reclamation state, so writer
  downgrades require a separately qualified recovery procedure.

  Previously corrupted stores require a separate recovery procedure.

## 1.25.1

### Patch Changes

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`01d2ebf`](https://github.com/reddb-io/reddb/commit/01d2ebfe3d8679bf67f136aed7d34f6331ccb47a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Reclaim obsolete table history in bounded background batches for writable
  standalone embedded runtimes. Avoid copying row payloads during VACUUM and
  planner statistics collection, and bound cardinality and histogram working sets.
  Rebalance deleted B-tree pages iteratively so cascading merges do not retain
  page buffers from every level on a maintenance worker's stack.
  Reuse trees after completed physical delete batches instead of re-encoding
  unchanged survivors at the next checkpoint, with revision checks that retain
  full validation after untracked edits.

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`9b80b83`](https://github.com/reddb-io/reddb/commit/9b80b83b31d72efaa7e4c2d4d1d1bd1ff39abe87) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Fence bulk row insertion through its WAL capture or append so a checkpoint cannot publish an unfinished batch that starts during serialization. Add forced-crash regressions for explicit transactions and acknowledged autocommit batches.

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`eaa4edf`](https://github.com/reddb-io/reddb/commit/eaa4edf7dfbfa28daf9e06bb9240d1535237850e) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Fence row mutations and delete stamps through their WAL append against checkpoint
  publication. Transactions starting during a checkpoint cannot leak uncommitted
  rows, and concurrent acknowledged autocommit updates retain their recovery WAL.

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`633925e`](https://github.com/reddb-io/reddb/commit/633925e76a67096784565a4b5d2e2b1d09dc7654) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Exclude unfinished row versions from crash-recoverable checkpoints and retain
  their committed preimages. Invalidate checkpoint reuse when transaction outcomes
  change, and fence commit WAL publication with visibility so a concurrent
  checkpoint cannot discard a durable commit's recovery records.

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`977b100`](https://github.com/reddb-io/reddb/commit/977b10025a44bd9e74ceea8e4c5ef3fd739034d6) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Restore shared column names on owned reads of reopened positional rows. Preserve
  untouched values during SQL updates and include positional rows when rebuilding
  secondary indexes without changing the file format or copying payloads again.

- [#2434](https://github.com/reddb-io/reddb/pull/2434) [`fbbfb40`](https://github.com/reddb-io/reddb/commit/fbbfb40827060ffcdf37c3ab99541fc9778dcf53) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Keep captured transaction inserts and row upserts out of published B-tree pages until their committed WAL is durable. This prevents incidental pager flushes from recovering unfinished inserts or delete stamps after a crash. Add crash regressions for INSERT, bulk INSERT, UPDATE and DELETE with open, rolled-back and committed transactions.

## 1.25.0

### Minor Changes

- [#2440](https://github.com/reddb-io/reddb/pull/2440) [`85bd8e8`](https://github.com/reddb-io/reddb/commit/85bd8e8cdb998f8c67086d6c406a55b79a0c140d) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Release the KV/SQL performance and durable-write cycle as 1.25: publish
  single-file KV replacements atomically, share durable grouped commits, avoid
  redundant overflow checkpoints and reserve bounded WAL headroom. Preserve both
  durability barriers, fence uncertain publication failures until reopen, and
  restore indexed SQL/KV reads and recovery metadata. Existing storage and wire
  formats remain unchanged. Full snapshots still cause pauses; measured results
  and upgrade boundaries are documented in docs/release/v1.25.0.md.

### Patch Changes

- [#2431](https://github.com/reddb-io/reddb/pull/2431) [`51d4a6d`](https://github.com/reddb-io/reddb/commit/51d4a6deeaf73c63564b31415cd5e8d0c0de139b) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Bound materialization for simple table LIMIT/OFFSET windows and avoid reserving
  memory for skipped rows or arbitrarily large requested limits.

- [#2439](https://github.com/reddb-io/reddb/pull/2439) [`6eb66dc`](https://github.com/reddb-io/reddb/commit/6eb66dc7a6cfa05b31961d3fad6324923c0c6b95) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Enforce the configured read-result byte ceiling with bounded JSON serialization,
  add materialized-read concurrency admission, and apply cooperative query deadlines
  to existing read work checkpoints. Output streams charge cumulative payload bytes.
  Mutation acknowledgements remain outside read-result limits; operator/cgroup and
  materialization limits remain necessary for memory and non-cooperative work.

- [#2428](https://github.com/reddb-io/reddb/pull/2428) [`bd94c3d`](https://github.com/reddb-io/reddb/commit/bd94c3d0590342d7a24999719a5dba3605e0f1f6) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Use segment key indexes for KV point reads instead of materializing entire collections, while retaining MVCC version selection and recovery behavior.

- [#2432](https://github.com/reddb-io/reddb/pull/2432) [`a3af4d6`](https://github.com/reddb-io/reddb/commit/a3af4d6261f05fcc2dccd5caca178c5ddf6936a3) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Extract KV read values directly from indexed candidates instead of cloning whole rows, preserving snapshot visibility and mutation behavior.

- [#2433](https://github.com/reddb-io/reddb/pull/2433) [`dd8fcc1`](https://github.com/reddb-io/reddb/commit/dd8fcc19da3c3274ef354e28daf447e919e37b93) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Retain active statement snapshot horizons through query completion so concurrent
  VACUUM cannot reclaim versions still needed by ordinary or nested reads.

- [#2441](https://github.com/reddb-io/reddb/pull/2441) [`9ea24de`](https://github.com/reddb-io/reddb/commit/9ea24de1acb13edbe97cd2b47410387afb0fda18) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Update rustls-webpki to 0.103.16 and webpki-roots to 1.0.10 across the workspace,
  Python driver and Node addon lockfiles. Reject distrusted emSign intermediate
  keys and remove the affected bundled root, addressing RUSTSEC-2026-0382 and
  RUSTSEC-2026-0381. Connections using affected certificate chains may now fail
  verification; replace the chain rather than disabling verification.

- [#2429](https://github.com/reddb-io/reddb/pull/2429) [`2169185`](https://github.com/reddb-io/reddb/commit/2169185b50d0915cb384b30d04f3994d86eba052) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve row versions required by snapshot transactions that excluded an older
  writer. VACUUM now retains that snapshot's horizon after the writer commits and
  releases it when the reader commits or rolls back.

## 1.24.15

### Patch Changes

- [#2426](https://github.com/reddb-io/reddb/pull/2426) [`c7ad27f`](https://github.com/reddb-io/reddb/commit/c7ad27f777d02f013745094fd92458af401b5aa0) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve aborted transaction outcomes during VACUUM so rolled-back inserts cannot
  become visible again, including in collections outside a targeted vacuum.

## 1.24.14

### Patch Changes

- [#2423](https://github.com/reddb-io/reddb/pull/2423) [`9aab8ea`](https://github.com/reddb-io/reddb/commit/9aab8ea1accaed0fcdf6d9ef31d4d675f811d7bc) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Reject main-file-only remote backups of existing paged stores even when they are opened with embedded options. Those stores also require physical metadata sidecars; restoring only the main file can lose declared collection models. Require the existing complete, fenced physical-directory backup path rather than advertising an incomplete snapshot as successful. Self-contained embedded backups remain supported.

## 1.24.13

### Patch Changes

- [#2422](https://github.com/reddb-io/reddb/pull/2422) [`aad592b`](https://github.com/reddb-io/reddb/commit/aad592be509b4a315ca7c28e570fd5ab3b02b60c) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Build each catalog model snapshot from one physical metadata observation instead of reloading it independently for indexes, graph projections and analytics jobs. Serverless readiness checks evaluate native completeness directly without computing unrelated metadata-repair diagnostics. Compute aggregate readiness from one health observation and reuse that observation in catalog HTTP responses, instead of recomputing the complete health report for each scalar flag. Preserve index reconciliation, readiness criteria and fallback behavior while reducing repeated work and publication-lock waits during checkpoints.

  Reuse the caller's catalog snapshot for health and consistency diagnostics, and release the runtime connection-pool lock before storage inspection.

- [#2420](https://github.com/reddb-io/reddb/pull/2420) [`94f3a01`](https://github.com/reddb-io/reddb/commit/94f3a01fdf20d605f27b67bf29e91dd3abd626ef) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve collection definitions when embedded options open an existing paged database. Persist and remove those contracts through the physical metadata path so document updates retain their declared model after checkpoint and restart.

## 1.24.12

### Patch Changes

- [#2418](https://github.com/reddb-io/reddb/pull/2418) [`d16d332`](https://github.com/reddb-io/reddb/commit/d16d3329c293380c454a285495cd12451640afbc) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Fence WAL offset reset against outstanding durable commit acknowledgements and
  flusher state publication. A checkpoint could previously discard an enqueued
  record or reset its offsets before a waiting writer observed durability, leaving
  that writer waiting for an offset in the previous WAL. Allow the flusher to drain
  existing commit leases while reset is waiting, and prevent stale flush results
  from being published after reset. The WAL format is unchanged.

## 1.24.11

### Patch Changes

- [#2415](https://github.com/reddb-io/reddb/pull/2415) [`8b12481`](https://github.com/reddb-io/reddb/commit/8b12481195dd8444f9012cb0921a0e1eec26507c) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Keep physical metadata, health and authority inspection from publishing native
  artifacts or healing metadata files. Preserve explicit maintenance and recovery
  as the paths that publish repairs.

  Drain periodic background workers when the last foreground runtime or connection
  closes, so an active weak upgrade cannot retain the database lock after close.
  Unpark idle workers rather than waiting for their next timer tick.

  Reject paged RDDB stores in embedded SCRUB and salvage before scanning payloads.
  Recognize either paged superblock so a damaged primary cannot cause embedded
  recovery to misinterpret stored values or load a large paged file for carving.

- [#2417](https://github.com/reddb-io/reddb/pull/2417) [`ecc8954`](https://github.com/reddb-io/reddb/commit/ecc8954d2c986c5cabc8fe4cde504696e60a948a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Honor the stderr destination for slow-query events without creating fallback
  files. If a configured slow-query file or its directory cannot be opened,
  report the error and use stderr instead of panicking during database startup.
  Keep sampling, the bounded nonblocking writer and the telemetry ring store.

## 1.24.10

### Patch Changes

- [#2412](https://github.com/reddb-io/reddb/pull/2412) [`7d1328a`](https://github.com/reddb-io/reddb/commit/7d1328a638dd96a531f4ef44ceda94e340572c3a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Keep catalog and health observations from cloning collection payloads or rebuilding
  native search artifacts. Build native artifacts once per physical publication,
  and report health's metadata validation scope explicitly. Preserve descriptor
  consistency checks and leave full artifact decoding to explicit inspection.

  Honor CREATE INDEX IF NOT EXISTS before rebuilding an existing index, and fail
  opening a paged store when a collection root or entity payload cannot be decoded
  instead of silently returning a partial database.

## 1.24.9

### Patch Changes

- [#2410](https://github.com/reddb-io/reddb/pull/2410) [`45c5330`](https://github.com/reddb-io/reddb/commit/45c5330b20dbf4916f8284deb1049725e12b96a9) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Keep a failed consumed-freelist-trunk fence pending across allocation retries.
  Synchronize both superblocks before reusing any loaded free page after an I/O
  error, so retrying an allocation cannot bypass the durable ownership change.

- [#2408](https://github.com/reddb-io/reddb/pull/2408) [`45979e9`](https://github.com/reddb-io/reddb/commit/45979e95214af10014b609e07c6ce4cacea71838) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Reuse retired paged checkpoint generations after durable catalog publication,
  preserving active readers and retirement roots across restarts. Bound checkpoint
  and WAL-reconciliation payload batches, reuse free pages for allocator metadata,
  and fence consumed freelist ownership before reuse and superblock recovery.

## 1.24.8

### Patch Changes

- [#2405](https://github.com/reddb-io/reddb/pull/2405) [`725daac`](https://github.com/reddb-io/reddb/commit/725daacc9cd760b8cd56c32b24b5b4033b4a4e94) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve stored segment sequence identities when opening and replaying paged stores. Serialize automatic inserts after the segment assigns their identity, borrowing the final row instead of cloning its payload. Keep completed inline inserts reusable at checkpoints only when exactly that fenced mutation changed the manager; retain full comparison for untracked edits, publication changes and versioned rows. Seed reuse after durable WAL tree reconciliation to avoid immediately rebuilding recovered history. WAL acknowledgement, visibility checks and durable catalog publication remain required.

## 1.24.7

### Patch Changes

- [#2402](https://github.com/reddb-io/reddb/pull/2402) [`dfa2d5d`](https://github.com/reddb-io/reddb/commit/dfa2d5d90224dfdadf22542f1912c1392bace5ac) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Avoid cloning complete collections while computing checkpoint metadata fingerprints and catalog observations used by DDL validation. Visit borrowed rows in the existing stable ID order for fingerprints, retaining identical values with compact row references instead of duplicate payloads. Keep unchanged collection checkpoints reusable when rollback or vault visibility changes affect only other collections, using a shared frozen visibility snapshot and borrowed row-header checks.

## 1.24.6

### Patch Changes

- [#2400](https://github.com/reddb-io/reddb/pull/2400) [`0873711`](https://github.com/reddb-io/reddb/commit/0873711ffb2a1468ad3c71a387d530edc996d049) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Reuse unchanged paged collection trees and physical metadata fingerprints during checkpoints. Track segment mutations, rollback visibility and pending vault publication so checkpoint reuse preserves acknowledged data and existing durable publication ordering.

## 1.24.5

### Patch Changes

- [`441547f`](https://github.com/reddb-io/reddb/commit/441547f17c5ba1428027d3f3e5bca3e70321d3de) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve the published tree generation when a paged checkpoint fails. Build changed trees separately, synchronize their pages before publishing the catalog, and reuse unchanged trees to avoid rewriting the database on every checkpoint. Serialize borrowed entities to reduce checkpoint memory overhead.

## 1.24.4

### Patch Changes

- [`b11be99`](https://github.com/reddb-io/reddb/commit/b11be992a97462c37c5b24929772379f31caf7b6) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Preserve dirty data pages evicted while publishing freelist trunks during checkpoint. Include them in the WAL-first, double-write-protected flush batch to prevent acknowledged data from reverting after a cold reopen.

## 1.24.3

### Patch Changes

- [#2396](https://github.com/reddb-io/reddb/pull/2396) [`fbf420c`](https://github.com/reddb-io/reddb/commit/fbf420c9386325a3528757e53e4ec8a5349c8916) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Replay committed paged WAL actions into memory before building replacement trees, and synchronize replacement pages before publishing their catalog roots. Interrupted recovery no longer spills intermediate historical tree mutations over the published checkpoint. Unchanged trees are retained, auxiliary WAL records remain available to runtime recovery, and old metadata overflow pages are recycled only after the replacement catalog is durable.

  Balance leaf splits by occupied bytes so mixed-width records fit on both pages during replay and live writes.

## 1.24.2

### Patch Changes

- [#2394](https://github.com/reddb-io/reddb/pull/2394) [`c6b0e84`](https://github.com/reddb-io/reddb/commit/c6b0e848277d41af36c524b77f2219e58af36904) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Recover the pager allocation floor from the physical file extent before allocating after a crash. Pages written by cache eviction or double-write recovery can reach disk before the allocation counter in the superblock; reopening could otherwise allocate over those existing pages. Partial trailing pages are reserved without being trusted, and read-only opens do not change the file.

## 1.24.1

### Patch Changes

- [#2392](https://github.com/reddb-io/reddb/pull/2392) [`f4c2a02`](https://github.com/reddb-io/reddb/commit/f4c2a02cfbde85db3c0129e73383e73fb499e1f3) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Concurrent DDL no longer corrupts the internal B-trees (`red.control_events`, `red_index_registry`, `red_stats`). Several processes running the same idempotent migration at once could leave every later `CREATE TABLE` / `ALTER TABLE` failing, or wedge the server at capacity. DDL statements now run one at a time; a DDL that cannot get the catalog lock within `concurrency.locking.deadlock_timeout_ms` fails with a retryable error instead of running unlocked. Single-row writes into one table no longer race each other inside its B-tree, and the physical metadata file is replaced atomically.

## 1.24.0

### Minor Changes

- [#2357](https://github.com/reddb-io/reddb/pull/2357) [`88dfc21`](https://github.com/reddb-io/reddb/commit/88dfc2183a40f462557694e476cab1ca83513d53) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Drop the `postinstall` download from `@reddb-io/cli` and `@reddb-io/sdk`. The `red` binary now ships as per-platform npm packages (`@reddb-io/red-linux-x64`, `-linux-x64-musl`, `-linux-arm64`, `-linux-arm64-musl`, `-linux-arm`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`) declared as `optionalDependencies`, so the package manager installs only the one for your OS/CPU/libc. Installs run no script and need no network beyond the registry, and work with `--ignore-scripts`. Don't install with `--omit=optional`. `REDDB_BIN` still overrides; `REDDB_SKIP_POSTINSTALL`, `REDDB_POSTINSTALL_VERSION` and `REDDB_POSTINSTALL_REPO` are gone.

- [#2391](https://github.com/reddb-io/reddb/pull/2391) [`a88cab4`](https://github.com/reddb-io/reddb/commit/a88cab431df12fc720ca9cff377fdc3cfb53dae2) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Expose the native Blob Cache over authenticated HTTP with tenant-scoped namespaces, hard TTL, binary values, existence probes and key/prefix/tag/namespace invalidation. The remote JavaScript client now uses application-authorized endpoints for tag invalidation and namespace flush, without requiring an operator credential. Align cache policy types with the implemented HTTP contract.

### Patch Changes

- [#2337](https://github.com/reddb-io/reddb/pull/2337) [`5554c50`](https://github.com/reddb-io/reddb/commit/5554c5049bbf40440330cb956b2bf4c064528e85) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Refuse a second writer process on an embedded single-file `.rdb` instead of letting the last one to close silently discard the other's commits, and reuse checkpoint space so the file no longer grows by a full snapshot on every open/close.

- [#2339](https://github.com/reddb-io/reddb/pull/2339) [`8481d83`](https://github.com/reddb-io/reddb/commit/8481d831a7461e3f2111f8bb2cd9a14b46d7dfbd) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Skip republishing an embedded `.rdb` snapshot that is byte-identical to the durable one over an empty WAL, so closing a store no longer rewrites the whole image two or three times.

- [#2371](https://github.com/reddb-io/reddb/pull/2371) [`c59f367`](https://github.com/reddb-io/reddb/commit/c59f3677205d0685b0f261d35124adbb7bf95c62) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Fix an unauthenticated remote crash: a SQL statement containing a multi-byte character (for example `SELECT 'héllo'`) made the server panic and, because release builds abort on panic, exit. The keyword matchers that sliced the statement at a fixed byte offset now return "no match" instead. The SPARQL parser had the same class of bug (its byte cursor advanced by 1 after reading a multi-byte character) and could additionally loop forever, growing memory without bound, on any token it did not recognise (`;`, `!`, an emoji); both are fixed.

- [#2338](https://github.com/reddb-io/reddb/pull/2338) [`9acffe7`](https://github.com/reddb-io/reddb/commit/9acffe78f0dfa0999e37c391acfb045380bcd36c) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Stop an embedded store from growing on every open: config writes now replace a key's earlier value instead of appending a row, and reading a config key returns its latest value.

- [#2340](https://github.com/reddb-io/reddb/pull/2340) [`f94069f`](https://github.com/reddb-io/reddb/commit/f94069fc4c6596bc8fd69dd0dcf3f4cd751100d1) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Skip config writes that would store the value a key already holds, so opening an embedded store no longer issues dozens of durable WAL appends and fsyncs to re-seed unchanged config.

- [#2384](https://github.com/reddb-io/reddb/pull/2384) [`2e44b72`](https://github.com/reddb-io/reddb/commit/2e44b72adb3831940deafe6ffe66d21d9d3f2aae) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Standalone RedWire TLS now uses the runtime authentication store. Valid session tokens work over `reds://`, and authenticated servers reject anonymous connections instead of treating the TLS listener as an unauthenticated server.

- [#2387](https://github.com/reddb-io/reddb/pull/2387) [`ce1d71b`](https://github.com/reddb-io/reddb/commit/ce1d71b6d2958b3c8c2bf64de82a30b21b73e0df) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Refuse online operational-directory backups that would omit the required physical sidecars, while retaining the existing single-file backup and restore path.

- [#2385](https://github.com/reddb-io/reddb/pull/2385) [`05e7f58`](https://github.com/reddb-io/reddb/commit/05e7f589e37ac1ada27cb9b3d3fc554c36118105) Thanks [@filipeforattini](https://github.com/filipeforattini)! - S3 backup requests can use a mounted, atomically rotated JSON file with scoped temporary credentials. Each request signs the current session token and rejects unavailable, expired or mismatched credentials without falling back to static keys.

- [#2377](https://github.com/reddb-io/reddb/pull/2377) [`3c55719`](https://github.com/reddb-io/reddb/commit/3c55719795ae5b3784b8b6260bee8f2f65a392c3) Thanks [@filipeforattini](https://github.com/filipeforattini)! - `CREATE UNIQUE INDEX` is now enforced for BTREE, the default method, as well as HASH, for single-column and composite keys. It was accepted and listed as unique but never checked, so duplicate keys went in silently. The check covers INSERT, `ON CONFLICT`, bulk and columnar inserts, and now UPDATE: an UPDATE onto an existing key is rejected, as is a statement whose rows would collide with each other (`UPDATE t SET id = 9`). A NULL key never conflicts with another NULL, as in a composite key and in a declared UNIQUE constraint; a single-column unique HASH index used to reserve the NULL key. `CREATE UNIQUE INDEX` over rows that already hold a duplicate key is refused, and the dead versions left by earlier UPDATEs or DELETEs no longer count as duplicates. Dropping a BTREE index also drops its lookup pocket, so an index of the same name can be recreated. Reopening a database never refuses: an index created by an older release over data that already holds duplicates is rebuilt as it was, and the duplicates stay until they are fixed. Data already duplicated by older releases is not repaired.

- [#2375](https://github.com/reddb-io/reddb/pull/2375) [`e69edf1`](https://github.com/reddb-io/reddb/commit/e69edf1d948a091319aedecafb3657918c47a988) Thanks [@filipeforattini](https://github.com/filipeforattini)! - A composite `CREATE UNIQUE INDEX … USING HASH` now keys on every column. It keyed on the first column only, so `(x,'2')` was rejected as a duplicate of `(x,'1')`. A row with a NULL or missing column has no key and never conflicts, as with declared UNIQUE constraints. `UNIQUE` combined with `USING BITMAP`, `SPATIAL` or `H3` is now a parse error instead of being accepted and never enforced. Single-column indexes keep their key encoding, so existing indexes, which are rebuilt from the table rows when a database opens, are unaffected.

## 1.23.4

### Patch Changes

- 5ac018a: Fix missing RedWire CLI SELECT rows by using the existing typed full-result query path instead of interpreting a summary-only reply as rows. Pin actual column/value output in the cross-binary regression for all row formats. Includes the accumulated 1.23.3 changes, whose publication was stopped during artifact verification; see the v1.23.4 upgrade notes for persisted expression/function compatibility and memory-evidence limits.

## 1.23.3

### Patch Changes

- Reduce graph and exact-vector query allocations, enforce shared memory admission before mutation and graph expansion, and preserve logical identity, MVCC, RLS and recovery behavior. Context expansion now deduplicates ordered physical-edge streams with constant ID state and bounds retained candidates by the edge allowance. Includes the accumulated query/transport fixes and persisted collection-expression and invoker-function building blocks on main. See the v1.23.3 upgrade notes before enabling persisted expressions or functions; older writers must not be used on databases relying on those contracts. Allocation/admission regressions do not establish competitive latency or throughput parity.

## 1.23.2

### Patch Changes

- [#2069](https://github.com/reddb-io/reddb/pull/2069) [`03bb1bc`](https://github.com/reddb-io/reddb/commit/03bb1bcd6476e4d555fa9434f5d47bd337ae88da) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Expose the reversible legacy sidecar-to-zoned store conversion as `red migrate-pager-zone --path <FILE>` so applications can safely upgrade pre-1.23 databases before opening them.

## 1.23.1

### Patch Changes

- Hot-path performance sweep (PRs [#2010](https://github.com/reddb-io/reddb/issues/2010), [#2012](https://github.com/reddb-io/reddb/issues/2012), [#2014](https://github.com/reddb-io/reddb/issues/2014)): allocation-free probe keys and group-by steady state, `entry()`-based single lookups, `swap_remove`/`retain`/`drain` in maintenance structures, top-K without full sorts, `Var` interned behind `Arc<str>`, O(1) zero-alloc plan-cache hits, columnar transpose by move, and a borrowed WAL encode path (byte-format unchanged, proven byte-identical). Zero-alloc ratchet extended to cover the new hot paths. No API, wire, or on-disk format changes.

## 1.23.0

### Minor Changes

- Geo/spatial minor: disk-resident H3 spatial indexing and composable geofencing.

  - **GEO_WITHIN(point, POLYGON(...))** — point-in-polygon geofencing as a plain
    boolean predicate that composes with the rest of the WHERE clause (AND/OR/NOT),
    planner-accelerated through the H3 polyfill cover with EXPLAIN visibility
    (PRD [#1939](https://github.com/reddb-io/reddb/issues/1939)).
  - **POLYGON(...)** — variadic polygon constructor over a flat lat/lon list.
  - **Disk-resident H3 spatial index** over the disk B-tree, retiring the in-RAM
    rstar index: H3_INDEX, H3_RING, H3_TO_LATLNG, nearest-neighbour and radius
    search backed by the pager (PRD [#1932](https://github.com/reddb-io/reddb/issues/1932)).
  - **ORDER BY computed aliases** — sorting by a computed projection alias
    (e.g. `GEO_DISTANCE(...) AS dist ... ORDER BY dist`) is now deterministic;
    the sort materializes ORDER-BY-referenced computed keys before ordering
    ([#1982](https://github.com/reddb-io/reddb/issues/1982)).
  - **GeoJSON `Point` recognition** — document fields holding a GeoJSON
    `{"type":"Point","coordinates":[lon, lat]}` value are recognized as
    geographic points end to end (H3 indexing, spatial search, coverage
    hints), read longitude-first per RFC 7946; other GeoJSON geometries
    stay out ([#1943](https://github.com/reddb-io/reddb/issues/1943)).
  - Geo scalars catalogued and documented: GEO_DISTANCE, GEO_BEARING, HAVERSINE,
    VINCENTY, GEO_WITHIN, POLYGON — llms.txt and the spatial guide regenerated
    from the engine's own catalog.
  - Didactic storage-integrity read errors: checksum failures name the
    zone/page/collection, fail closed, and map to data-corrupted SQLSTATEs
    at the wire ([#1963](https://github.com/reddb-io/reddb/issues/1963)).

## 1.22.0

### Minor Changes

- Document DML overhaul — UPDATE surface + nested SET (ADR 0067, PRD [#1703](https://github.com/reddb-io/reddb/issues/1703), completing the clean break started in v1.21.0):

  - **UPDATE model markers removed.** `UPDATE <c> DOCUMENTS SET …`, `UPDATE <c> ROWS SET …`, and `UPDATE <c> KV SET …` are **rejected** with a didactic error — the catalog already knows the collection's model, so unmarked `UPDATE <c> SET …` infers it. `NODES`/`EDGES` markers stay (a graph holds both record kinds). Dotted assignment targets (`SET a.b.c = …`) now parse for all targets and are validated by the analyzer against the catalog model.
  - **Nested document SET** deep-merges dotted paths into the document body (intermediate objects created only where absent, siblings preserved), guarded against dotted paths through scalar/array intermediates and respecting the reserved-field rule.
  - **Docs re-canonicalized**: the Documents teaching surface uses the inline/unmarked forms throughout, with a new Identifier-vs-data case-rule section (identifiers fold case; JSON body keys are matched exactly).

  All published drivers (js, js-client, cpp, dart, dotnet, go, java, kotlin, php, python, python-asyncio, zig, client) emit the unmarked document UPDATE form in this release.

## 1.21.0

### Minor Changes

- Document DML overhaul (ADR 0066/0067, PRD [#1703](https://github.com/reddb-io/reddb/issues/1703)). The document write surface is redesigned around one canonical form:

  - **Inline strict-JSON literals are the only way to write JSON in RQL** — `INSERT INTO events DOCUMENT VALUES ({"level":"info"})`. The quoted-string coercion (`VALUES ('{…}')`) and the ceremonial `(body)` column list are rejected with didactic errors that name the sanctioned form; legacy `_ttl` metadata columns point at `WITH TTL`.
  - **`JSON_PARSE(<expr>)`** added as the explicit string→JSON conversion for runtime-string cases.
  - **Array literals parse losslessly** into a native array; vector-vs-JSON is resolved from the target's type at analysis/runtime instead of committing to an f32 vector at parse time (large integers destined for JSON no longer corrupt).
  - **Reserved envelope field names** (`rid`, `collection`, `kind`, `tenant`, `created_at`, `updated_at`) are enforced against user data at write time, with an error that names the field, the reserved set, and the rename recourse.
  - **The `DOCUMENT` marker is an optional idempotent model assertion**: unmarked `INSERT INTO c VALUES ({…})` infers the model from the catalog (existing document collection → document write; existing non-document → model error; unknown → didactic error), and `DOCUMENT` bootstraps-or-validates the collection's model.

  Published JS, js-client, and PHP/Python drivers flip to the inline form in this same release (no deprecation window — stated maintainer policy).

## 1.20.0

## 1.18.0

### Minor Changes

- minor release

## 1.17.0

### Minor Changes

- [#1517](https://github.com/reddb-io/reddb/pull/1517) [`f1f286d`](https://github.com/reddb-io/reddb/commit/f1f286ddfa034ad07826cd1bf75ab8941c21b2ac) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Harden release provenance with aggregate checksum manifests, GitHub Artifact
  Attestations, GHCR provenance/SBOM attestations, expanded release verification
  notes, and public support/release policy docs.

## 1.16.0

### Minor Changes

- testing maturity checkpoint

## 1.13.0

### Minor Changes

- [`99c9d9b`](https://github.com/reddb-io/reddb/commit/99c9d9bfa7ce40351d38d71cec8da752dd331745) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Centralize operational topology, monorepo contract authorities, client routing membership, queue lifecycle delivery, and parameterized statement execution.

## 1.12.0

### Minor Changes

- [#1308](https://github.com/reddb-io/reddb/pull/1308) [`a474ddb`](https://github.com/reddb-io/reddb/commit/a474ddb12045258a755d0c485974300838e6347a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Cluster bootstrap & operational telemetry groundwork.

  - **Cluster bootstrap authority**: fail-closed seam for cluster-shaped auth bootstrap ([#1229](https://github.com/reddb-io/reddb/issues/1229)), real auth-store wiring for cluster vault first boot ([#1231](https://github.com/reddb-io/reddb/issues/1231)), write-if-absent initial config on fenced bootstrap manifest apply ([#1232](https://github.com/reddb-io/reddb/issues/1232)), bootstrap-completion marker observed at boot through the authority seam ([#1230](https://github.com/reddb-io/reddb/issues/1230)), and cloud policy-first bootstrap manifest protections ([#1233](https://github.com/reddb-io/reddb/issues/1233)).
  - **Helm/Compose**: cluster bootstrap contract documented and render-checked; cluster members carry no bootstrap credentials, gated cluster auth/vault path with fail-closed messaging ([#1234](https://github.com/reddb-io/reddb/issues/1234)), plus duplicate/concurrent bootstrap drills ([#1235](https://github.com/reddb-io/reddb/issues/1235)).
  - **Durability**: collision-proof WAL backup temp paths so segment digests stay honest ([#1294](https://github.com/reddb-io/reddb/issues/1294)).
  - **Operational telemetry**: Phase-0 substrate contract (ADR 0060) defining the store/read-model boundary, retention/cardinality budgets, and redaction rules ahead of the metric slices ([#1247](https://github.com/reddb-io/reddb/issues/1247)).
  - **Process**: binding merge gate + green ratchet on `main` (ADR 0059, [#975](https://github.com/reddb-io/reddb/issues/975)).

## 1.11.0

### Minor Changes

- [#1206](https://github.com/reddb-io/reddb/pull/1206) [`6f29076`](https://github.com/reddb-io/reddb/commit/6f29076fe701822d0f576fa9cf49ec5d67b8f94a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Centralize operational topology, monorepo contract authorities, client routing membership, queue lifecycle delivery, and parameterized statement execution.

## 1.10.1

## 1.10.0

## Unreleased

### Added

- `QueueLifecycle` gains three new methods on the canonical surface:
  `pop`, `delete_with_state`, `move_between_queues`
  ([#539 follow-up](https://github.com/reddb-io/reddb/issues/539),
  [#716](https://github.com/reddb-io/reddb/issues/716)). Lifecycle
  wrappers delegate to new `QueueStore` trait methods
  (`pop_available`, `delete_with_state`, `move_to_queue`) implemented
  on all three stores — `InMemoryQueueStore` (semantic implementation
  with proper tombstone routing), `PrimaryQueueStore` (shims to the
  existing `queue_delivery::*` MVCC-aware paths), and
  `ReplicaQueueStore` (returns `ReplicaImmutable`).

- Every queue command handler in `runtime/impl_queue.rs` (Pop, Peek,
  Purge, Read, Claim, Ack, Nack, Move) now routes through
  `QueueLifecycle` instead of `queue_delivery::*` directly
  ([#716](https://github.com/reddb-io/reddb/issues/716)). The lifecycle
  is the single funnel for every mutation on a queue; `queue_delivery`
  shrinks to an implementation detail of the primary store and will be
  fully inlined in a later slice. Wire output is byte-identical
  (existing transport tests unchanged).

- Domain vocabulary for **Live queue wait**, **Ephemeral notification**,
  and **Durable stream** + **ADR 0028** establishing the boundary
  between the three primitives so the queue lifecycle does not absorb
  incompatible semantics. First-cut `QUEUE READ ... WAIT` semantics
  scoped (autocommit-only, explicit duration, server-side max cap,
  Prometheus telemetry).

### Notes

- The `## Unreleased` entries below carried the NEON + QUEUE ACK
  delivery_id work shipped in v1.6.0; they have been moved under that
  release heading where they belong.

## 1.6.0

### Added

- TurboQuant NEON scoring kernel on the canonical blocked-by-32 layout
  ([#692](https://github.com/reddb-io/reddb/issues/692),
  [PRD #688](https://github.com/reddb-io/reddb/issues/688),
  [ADR 0024](.red/adr/0024-turboquant-encoded-storage-layout.md)). aarch64
  hosts now get SIMD scoring on TurboQuant — `select_scorer()` picks
  `NeonScorer` (NEON is mandatory in the AArch64 base ISA) and falls back to
  scalar on every other arch. The kernel reads aligned 128-bit lanes
  straight from `BlockedCodeStorage::block_codes(...)` with no per-query
  gather, uses `vqtbl1q_u8` for nibble lookups, and finishes with
  `vfmaq_f32` so output is bit-identical to `ScalarScorer` (the contract
  the AVX2/AVX-512BW slices already enforce). Equivalence test gated
  `#[cfg(target_arch = "aarch64")]`; release matrix already builds
  aarch64-gnu natively on a Blacksmith ARM runner and aarch64-musl via
  `messense/rust-musl-cross` so no new build deps are introduced.

- `QUEUE ACK` / `QUEUE NACK` accept the server-issued opaque `delivery_id`
  alongside the legacy `(queue, group, message_id)` tuple
  ([#627](https://github.com/reddb-io/reddb/issues/627),
  [ADR 0026](.red/adr/0026-delivery-id-wire-shape.md)). New RQL syntax:
  `QUEUE ACK <queue> [GROUP <group> '<message_id>'] [WITH delivery_id = '<base32>']`.
  The same wire shape applies to all four transports (redwire, gRPC,
  Postgres-wire, HTTP). When both handles are supplied, `delivery_id` wins
  unconditionally — a stale `delivery_id` returns an error instead of
  falling back to the tuple.

### Deprecated

- ACK/NACK by the legacy `(queue, group, message_id)` tuple. The tuple path
  still works in this release but emits a rate-limited server-side
  deprecation log line (per connection + queue, one entry per minute). The
  tuple path will be removed one minor release after this one — drivers
  should switch to sending `delivery_id`, which is already returned by
  `QUEUE READ` / `QUEUE CLAIM`.

## 1.5.0

### Minor Changes

- Policy-first auth, governance guardrails, NFS/build infra hardening, dep maintenance

  - feat(auth): policy-first evaluator — admin is no longer a bypass ([#644](https://github.com/reddb-io/reddb/issues/644))
  - feat(auth): managed policy guardrail tracer + system-owned user integration ([#646](https://github.com/reddb-io/reddb/issues/646), [#647](https://github.com/reddb-io/reddb/issues/647))
  - feat(auth): red.registry tracer + managed config namespace enforcement ([#648](https://github.com/reddb-io/reddb/issues/648), [#649](https://github.com/reddb-io/reddb/issues/649))
  - feat(auth): policy context for system-owned/platform-scoped users ([#645](https://github.com/reddb-io/reddb/issues/645))
  - feat(server): explicit --no-auth / --dev flag for local no-password mode ([#663](https://github.com/reddb-io/reddb/issues/663))
  - fix(engine): rid/logical-identity envelope regression + KV()-in-WHERE ([#636](https://github.com/reddb-io/reddb/issues/636))
  - fix(engine): parser unbounded recursion / OOM on view body + WITHIN ([#635](https://github.com/reddb-io/reddb/issues/635))
  - fix(engine): WAL recovery commit-batch replay, dotted-tenant scoping, $secret/$config SQL, ASK envelope fixes ([#638](https://github.com/reddb-io/reddb/issues/638), [#639](https://github.com/reddb-io/reddb/issues/639), [#640](https://github.com/reddb-io/reddb/issues/640), [#641](https://github.com/reddb-io/reddb/issues/641))
  - fix(gate): pnpm build now actually builds (was 90-min workspace-test); package.json scripts straightened out
  - chore(deps): bump grpc 1.79→1.81, protobuf 1.36.10→1.36.11, toml 0.8→1.1, criterion 0.5→0.8, lz4_flex, busybox

## 1.4.0

### Minor Changes

- a60b666: 1.4.0 minor release. Raises the storage engine page size from 4KB to 16KB
  (matching InnoDB's default) for higher B-tree fanout, and grows the maximum
  inline attribute value from 1024 to 4096 bytes (now derived as `PAGE_SIZE / 4`).

  **Breaking on-disk format change:** databases written by 1.3.x and earlier use
  4KB pages and will not open under 1.4.0 — there is no in-place migration. Treat
  this as a fresh-format release.

## 1.3.1

### Patch Changes

- [`cfce794`](https://github.com/reddb-io/reddb/commit/cfce794979cbfa68d16815974c70007dfa4bad16) Thanks [@filipeforattini](https://github.com/filipeforattini)! - 1.3.1 patch release. Re-publishes the 1.3.x line across all registries (npm,
  crates.io, GHCR, GitHub Release) after the 1.3.0 npm publish was blocked by a CI
  token issue. No functional change since 1.3.0 — the parser fix ([#635](https://github.com/reddb-io/reddb/issues/635)) and the
  `GRAPH COMMUNITY ... RETURN ASSIGNMENTS` feature ([#660](https://github.com/reddb-io/reddb/issues/660)) shipped in 1.3.0 are
  included here as well.

## 1.3.0

### Minor Changes

- [`162ed9b`](https://github.com/reddb-io/reddb/commit/162ed9bed3b7161d3e6e8f31de509eb376e16cf9) Thanks [@filipeforattini](https://github.com/filipeforattini)! - **Feature: per-node community assignment from `GRAPH COMMUNITY` ([#660](https://github.com/reddb-io/reddb/issues/660)).**
  `GRAPH COMMUNITY ALGORITHM louvain RETURN ASSIGNMENTS` now emits one row per
  node `{node_id, community_id}` — the node→community map needed to colour or
  visualise nodes by community. Without the `RETURN ASSIGNMENTS` clause the
  historical per-community aggregate shape (`community_id`, `size`) is unchanged
  (backward compatible). `LIMIT` caps the per-node rows.

- [#631](https://github.com/reddb-io/reddb/pull/631) [`b189b7e`](https://github.com/reddb-io/reddb/commit/b189b7eab9ea23553548007ec5106292a834e01f) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Promote the work merged since v1.2.5 to a minor release (1.3.0): new Analytics primitives + window functions are feature-level, so this is a minor, not a patch.

  **HTTP transport hardening ([#569](https://github.com/reddb-io/reddb/issues/569)).** Bounded handler concurrency via a deep
  `HttpConnectionLimiter` (hard cap → `503 Service Unavailable` + `Retry-After`
  on saturation, rejection before parse/routing), an overall per-handler
  wall-clock timeout that reclaims the limiter slot on expiry, the same shared
  cap enforced on the TLS accept loop (HTTP + HTTPS draw one cap), and Prometheus
  telemetry (`http_active_handler_threads`, `http_handler_rejected_total`,
  `http_handler_duration_seconds`, `http_handler_cap`). New config: `--http-max-handlers`,
  `--http-handler-timeout-ms`, `--http-retry-after-secs`.

  **QueueLifecycle foundations ([#527](https://github.com/reddb-io/reddb/issues/527) prereqs).** `QueueTxn` now participates in the
  caller's transaction via the runtime MVCC path so lifecycle ack/purge/delete are
  rollback-safe; the primary `QueueStore` adapter reads the legacy `queue_pending`
  state (closing the parallel meta-row divergence); and lifecycle gains
  `group_read` + `claim` methods that preserve the legacy `consumer`/`delivery_count`
  result shape. (The atomic cutover and the `delivery_id` wire handle remain
  follow-ups — the queue ACK/NACK contract is unchanged in this release.)

  **Analytics-event primitives ([#575](https://github.com/reddb-io/reddb/issues/575))** and **SDK Helper Spec v1.0 + cross-driver
  conformance ([#449](https://github.com/reddb-io/reddb/issues/449))** as previously merged on main.

  **Fix: embedding `linked_row_id` via rid envelope.** The row rid envelope
  refactor moved a query row's logical identity to the canonical `rid` key;
  `CLUSTER`/embedding writeback still keyed off the legacy `red_entity_id`
  column and silently dropped `linked_row_id`. Row→vector linkage is restored.

  Also documents that table column names persist across a file-backed reopen and
  that aggregate result columns use a single canonical `FUNC(arg)` casing —
  both verified by regression guards; these were 1.1.x-era reports already
  resolved on the 1.2 line.

### Patch Changes

- [`162ed9b`](https://github.com/reddb-io/reddb/commit/162ed9bed3b7161d3e6e8f31de509eb376e16cf9) Thanks [@filipeforattini](https://github.com/filipeforattini)! - **Fix: parser stack overflow + view filter desync ([#635](https://github.com/reddb-io/reddb/issues/635)).** Parsing a recursive
  view body could overflow the stack (the `parse_sql_command` match frame was
  oversized — extracted the CREATE arm to shrink it). Separately, the view
  rewriter merged the inner query's `filter`, but the executor prefers `where_expr`
  and nulled `filter` when present, so the merged predicate was silently dropped
  (`view_chain_resolves_recursively`); the rewriter now keeps `where_expr` in sync
  with the merged filter. Re-enables the previously quarantined view/materialized-view
  parser binaries ([#593](https://github.com/reddb-io/reddb/issues/593)/[#594](https://github.com/reddb-io/reddb/issues/594)/[#595](https://github.com/reddb-io/reddb/issues/595)/[#596](https://github.com/reddb-io/reddb/issues/596)/views).

## Unreleased

### Documentation

- New canonical SDK Helper Spec at `docs/spec/sdk-helpers.md` (v1.0). Defines
  the helper names (snake_case dot-namespaced), input shapes, output
  envelopes, error taxonomy, and per-model conformance cases for documents,
  KV, queues, transactions, vectors, graph, time-series, and probabilistic
  surfaces. The previous v0.1 draft at `docs/clients/sdk-helper-spec.md`
  now points at the canonical spec. A reference conformance harness ships
  in `crates/reddb-client/tests/conformance.rs`; other-language drivers
  port the case IDs verbatim. Refs #546.

## 1.2.0 - 2026-05-15

### Fixed

- Server and client Docker images now use `gcr.io/distroless/cc-debian13`
  so the glibc 2.39 binaries produced by the Ubuntu 24.04 release builders
  run on the runtime base. Prior 1.1.x images failed at startup with
  `version 'GLIBC_2.39' not found`.

### Breaking Changes

- Public item identity now uses the canonical RedDB ID field `rid` across
  rows, documents, KV, graph nodes, graph edges, vectors, events, CDC,
  HTTP/gRPC/MCP, and SDK-visible query results. Older public aliases such as
  `_entity_id`, `red_entity_id`, and `entity_id` are removed from public
  envelopes; graph edge endpoints are `from_rid` and `to_rid`.
- The item envelope fields `rid`, `collection`, `kind`, `tenant`,
  `created_at`, and `updated_at` are reserved system fields. New table schemas
  and supported top-level document, KV, node, and edge payloads that define
  those names fail with a reserved-field error.
- Multi-model updates are now explicit through `ROWS`, `DOCUMENTS`, `KV`,
  `NODES`, and `EDGES` targets. Compound assignment, `RETURNING`, and ordered
  `ORDER BY ... LIMIT` batches are documented as the canonical update surface;
  failed multi-item updates abort atomically without partial writes.
- Unaliased expression projections now use the rendered source expression as the
  result column name instead of the internal operator/function tag. For example,
  `SELECT UPPER(name)` now returns `UPPER(name)` and `SELECT id * 2` now returns
  `id * 2`, while explicit `AS <alias>` names still take precedence.

## 1.1.0

### Minor Changes

- [`9bef862`](https://github.com/reddb-io/reddb/commit/9bef862babe56cfbc75850c94c2b8f863a6fd8de) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Prepare the 1.1 line with parameterized query coverage across engine, transports, and drivers; ASK citation, streaming, cache, failover, audit, and gRPC/MCP surfaces; MVCC transaction recovery improvements; graph/vector/probabilistic query fixes; SDK helper APIs; and release asset hardening.

## 1.0.8

### Patch Changes

- [`c92f9e7`](https://github.com/reddb-io/reddb/commit/c92f9e7d386b459107a60a0796a25fce2c23ffc5) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Bundle of work that landed since `v1.0.7`.

  **Query / engine**

  - `SELECT … LIMIT $N OFFSET $N` parameterized — completes the `$N` bind sweep (`closes [#361](https://github.com/reddb-io/reddb/issues/361)`).
  - ASK responses now include inline `[^N]` citation markers, parsed by a dedicated `CitationParser` (`closes [#393](https://github.com/reddb-io/reddb/issues/393)`).

  **Drivers**

  - **Go.** `c.Query(ctx, sql, params...)` accepts variadic `any` bind values, routed through the binary `QueryWithParams` frame when the server advertises `FEATURE_PARAMS`. Full Go → engine type mapping documented in the driver hub page (`closes [#363](https://github.com/reddb-io/reddb/issues/363)`).
  - **JS / TS.** RedWire `QueryWithParams` frame emit + `FEATURE_PARAMS` capability negotiation in the SDK and thin client.

  **Infrastructure**

  - `lru` bumped from `0.12` → `0.16`, picking up the upstream fix for the `IterMut` Stacked Borrows UB flagged by Dependabot (alerts [#19](https://github.com/reddb-io/reddb/issues/19), [#20](https://github.com/reddb-io/reddb/issues/20)).
  - CI workflows replace `arduino/setup-protoc@v3` (Node 20-deprecated) with a local `install-protoc` composite action that downloads protoc 28.3 directly. Eliminates the deprecation warning on every job.
  - Dropped the unsupported `save-always: true` input from `Swatinem/rust-cache@v2` (silently ignored but emitted a warning per build).

## 1.0.7

### Patch Changes

- [`da708d9`](https://github.com/reddb-io/reddb/commit/da708d979ed3a2289dd5da985db48879504d6b94) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Skips the stalled `v1.0.6` release. This bundles everything that landed since `v1.0.5` plus the infrastructure that makes future releases atomic.

  **Query / engine**

  - `$N` positional parameters across the query language (INSERT VALUES, SEARCH SIMILAR, SEARCH HYBRID, SEARCH TEXT, SEARCH MULTIMODAL, SEARCH SPATIAL, SEARCH INDEX — `LIMIT $N`, `MIN_SCORE $N`, `K $N`).
  - `?` positional placeholders in the parser, with mixing detection.
  - HTTP `/query` now accepts a `params` JSON array.

  **Distribution & docs**

  - Workspace crates published under the `reddb-io-*` namespace on crates.io (`reddb-io`, `reddb-io-client`, `reddb-io-server`, `reddb-io-wire`, `reddb-io-grpc-proto`, `reddb-io-client-connector`). Rust library import paths (`use reddb::…`, `use reddb_client::…`, etc.) are unchanged.
  - Per-language driver pages under `docs/clients/drivers/` (rust, python, python-asyncio, go, php, dart, cpp, zig, bun) plus a hub matrix at `docs/clients/drivers.md`.

  **Release pipeline**

  - macOS Intel binaries (`red-macos-x86_64`, `red_client-macos-x86_64`) are now produced by the release matrix.
  - Adopted [Changesets](https://github.com/changesets/changesets) for atomic version + release: the version bump and the GitHub Release tag are now produced by CI in a single step, eliminating the race that caused `@reddb-io/sdk`'s postinstall to 404 in the window between a local `pnpm version` and the release workflow finishing.
  - `postinstall` scripts now (a) print actionable recovery paths when an asset 404s and (b) skip cleanly when running from a workspace checkout (`pnpm install` in the monorepo no longer surfaces a download error).

All notable changes to this project are documented here. Versions follow
[SemVer](https://semver.org/) and the engine + locked-step npm/PyPI
packages share one number (see `scripts/check-versions.sh`).

## 1.0.0 — 2026-05-06

First stable release. Engine, drivers, and CLI move to the `@reddb-io/*`
npm org and lock at `1.0.0`.

### npm migration (#125)

- `@reddb-io/cli` — CLI launcher (was `reddb-cli`).
- `@reddb-io/sdk` — JS/TS driver (RedWire/gRPC/HTTP/embedded).
- `@reddb-io/client` — thin remote-only driver, downloads `red_client`
  binary post-install (#136, #137, #138).
- `scripts/sync-version.js` propagates engine bumps to all locked-step
  packages including `drivers/js-client/`.
- `scripts/check-versions.sh` enforces lock-step in CI.
- Legacy `reddb-cli` npm package deprecated post-publish via
  `scripts/deprecate-legacy-npm.sh` (operator-triggered, see
  `docs/release-runbook.md`).

### Engine

- UnifiedRecord storage layout (perf push closure).
- WAL lock-free append path.
- Pager striped lock to remove the global page-cache contention.
- BTree batch ingest path.
- AggregateQueryPlanner for push-down aggregates.
- IncrementalIndex maintenance.
- AI batching final cleanup (#281): synchronous `openai_embeddings`,
  `openai_prompt`, and `anthropic_prompt` Rust helpers are deprecated.
  Internal server paths now use `AiBatchClient` or the pooled
  `AiTransport` path. The mock-provider AUTO EMBED validation for 1000
  rows drops provider fan-out from 1000 requests to 1 request when the
  provider max batch is 2048, reducing the provider-wait component from
  about 100s to about 0.1s at 100ms provider latency.

### Topology (#164)

- Topology discovery PRD substantially shipped: advertiser, consumer, and
  router landed; E2E coverage in flight (#172).
- Strategic gate ADR 0009 — gate is scenario-specific, not global.

### Out of scope for this slice

- The unscoped `reddb` npm name is owned by an unrelated upstream package
  and is not deprecated.
- `drivers/python-asyncio` and `charts/reddb` follow independent version
  policies.
