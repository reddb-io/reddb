# ADR 0077 — RedDB Browser, shared engine and OPFS

Status: accepted architecture; persistence, portable query operators, B-tree/pager, live MVCC and snapshot decoding shared; storage-backed runtime and sync pending
Date: 2026-10-22

## Decision

RedDB Browser will run a shared Rust engine in WebAssembly inside a dedicated
worker. Its first usable database release must support tables, documents, KV,
graph and vectors, offline writes, and synchronization of a server-authorized
subset. Conflicting writes are preserved for explicit application resolution.

The local database must work fully offline through a JavaScript/TypeScript SDK:
all five models, local queries, writes, transactions and recovery run without a
RedDB server, account, external API or CDN. Rust/WASM is bundled with the SDK,
not fetched from a provider at runtime. The host application supplies local
assets or caches its application shell and SDK/worker/WASM assets for offline
loading. A fresh worker and browser restart must work with the network disabled
after installation. Synchronization is optional and must never gate local use.

OPFS is the primary storage backend. IndexedDB may hold recoverable auxiliary
metadata; it must not split a critical transaction across two storage systems.
Data, pending write intent and sync progress share a transactional boundary.
The existing embedded single-file `.rdb` is the storage contract, owned by
`reddb-file`; browser code must not redeclare superblocks, manifests or WAL codecs.

The first implementation extracts native opening/locking/cached-tail policy from
the byte-I/O publication algorithms. `EmbeddedRdbFile<F: VfsFile>` consumes an
exclusively owned handle and an explicit creation timestamp. The native path and
WASM path call the same algorithms. OPFS `flush` backs the data and root barriers;
its guarantees remain subject to the browser and underlying storage. Native
locking, checksums, format versions and publication ordering remain in force.
Failed I/O invalidates a portable handle until reopen/recovery. Corrupt existing
files never trigger automatic truncation/recreation.

Web Locks plus an exclusive OPFS sync handle establish one writer. Request
forwarding between tabs is a later SDK step; this foundation returns `FILE_BUSY`
instead of allowing independent engines over the same file. Baseline execution
must not require shared WASM memory, cross-origin isolation headers, WebGPU or
background sync. Clearing site data removes local state. Background sync is an
optional optimization, not an availability or durability guarantee.

## Engine and synchronization boundaries

ADR 0053 keeps the physical executor coupled to native storage in `reddb-server`.
Browser requires extraction of that executor/pager boundary, with native and
browser conformance evidence; a second JavaScript evaluator is not an acceptable
shortcut. `reddb-engine` now owns the storage-independent scalar evaluator,
compiled row filters, sorting, binding algebra/iterators, joins, aggregation,
set operations, subqueries and windows, relocated with their native unit tests.
Native module paths re-export those exact types; runtime policy, authorization,
durable transaction publication and storage-backed query orchestration remain in
`reddb-server`.
Canonical output-column labels and binary document decoding also have one owner.

The page primitives now also live in `reddb-engine::storage`: 16 KiB pages,
CRC32, the freelist and SIEVE page cache. Native module paths alias those exact
types and implementations. Their persisted encodings still belong to
`reddb-file`; the move changes neither bytes nor cache/allocation algorithms.
Native unit tests moved with the code, including the native cache concurrency
tests. Shared native/WASM fixtures cover page headers and cells, checksums and
corruption, multi-trunk freelist recovery, pinning, eviction and dirty writeback.
A native pager restart test checks that shared page types cross the existing
file-I/O boundary.

`reddb-engine::storage::pager_io` now also owns raw page I/O, sealed superblock
slot writes and the in-file double-write staging/replay algorithms over
`VfsFile`. The native pager calls those same functions, borrowing its existing
locked file without cloning it. Staging sync precedes home writes, home sync
precedes zone clearing, and clearing is synced before the next frame. The file
mutex now spans a complete protected batch, preventing interleaved page writers
from overwriting its recovery frame. Metadata-read failures propagate as I/O
errors rather than looking like an empty superblock zone. Codecs, file versions,
allocation fences, native WAL-first policy and encryption remain unchanged.

Shared native/WASM tests interrupt every write/flush in page publication,
superblock publication and DWB recovery, retaining or losing pending writes.
They verify complete pages, the surviving root, retryable recovery and bounded
multi-frame writes. A separate test-only WASM fixture uses the production OPFS
bridge, tears a real home page after staging sync, closes the failed handle and
recovers through a fresh worker after a full offline browser restart. This is
page-level I/O recovery, not transaction or five-model execution. The production
SDK does not expose these pager fixtures.

Pager orchestration is now shared too: `reddb-engine::storage::pager::Pager<F, W>`
owns cached reads/writes, freelist publication, deferred reclamation, consumed-trunk
fences, mirrored reused-page ownership, physical-tail reservation and checkpoint
markers. The native pager retains its file-opening/locking/CoW probes, encryption
binding, optional native WAL writer, prefetch and flush-on-drop policy, and delegates
the page algorithms to this exact implementation. `WalBarrier` is the host's
synchronous durability boundary; a portable `NoWal` pager rejects nonzero page LSNs.
The native adapter retains the existing optional-WAL behavior; a poisoned WAL slot
now returns an I/O error instead of silently bypassing the barrier.

Shared native/WASM fixtures exercise dirty eviction, deferred reclamation, both
superblock fallback paths after reuse, every consumed-trunk ownership-fence
write/flush, WAL-before-data ordering and failure, partial physical tails and
checkpoint restart. The real OPFS fixture now opens the complete shared pager,
recovers a torn home page through a fresh worker after offline browser restart,
then completes the interrupted checkpoint and verifies it with another pager.
Native/portable/native interoperability includes allocation, reclamation and
checkpoint state. File codecs and versions are unchanged. These are pager
operations, not database transactions: storage-backed execution, the runtime
WAL implementation and MVCC still need extraction and browser integration.

The canonical B-tree, cursor, leaf/interior helpers, large-value layout and
compressed/overflow-chain algorithms now also live in `reddb-engine::storage`.
`PageStore` supplies the existing page operations and an optional prefetch hint;
the native pager and the portable owned pager implement it. Native public B-tree,
cursor, overflow and value-error paths alias these exact implementations. The
reentrant structural-writer lock and reclamation/reader pins retain their native
semantics; the browser baseline still uses a single dedicated worker and requires
no shared WASM memory. File codecs remain in `reddb-file`, and no persistent
format or version changes are introduced.

Shared native/WASM fixtures cover ordered scans and seeks, mixed-width splits,
compressed values, multi-page overflow, point reads, batch updates, deletion,
reclamation and cursor lifetime. A test-only native host creates an actual paged
B-tree file; the browser imports it, verifies and mutates the tree with the host
HTTP listener stopped, then restarts entirely offline and exports the file for
native verification. A native-server integration additionally proves its public
B-tree aliases produce and consume the same records through the portable pager.
The test records include opaque values and encoded entities described below. The production
JS/TS API is unchanged: these lower-level reads/writes do not supply collection
catalog semantics, RQL storage execution, runtime MVCC integration, atomic application transactions,
or recovery of partially executed B-tree operations. Those runtime/WAL layers
still need extraction before the application SDK can be released.

Live optimistic MVCC state now lives in `reddb-engine::transaction`, including
snapshot allocation, visibility, aborted outcomes, snapshot retention/pins,
checkpoint exclusions, unique-key admission, SSI read/write tracking and the
per-connection transaction/savepoint/tenant state machine. Native paths re-export
the same types, including the shared entity identity. ADR 0065's dormant
coordinator/lock/log/savepoint scaffold remains disabled. The native savepoint
error classification and message remain unchanged through an adapter conversion.
The commit preparation callback still precedes visibility and its returned
publication guard remains held through child and parent visibility changes.
The host retains first-committer-wins validation, pending-write finalization,
WAL append/barriers and recovery; this extraction does not provide those pieces.

Nine native/WASM conformance cases exercise statement versus transaction
snapshots, active/aborted creators and deleters, failed preparation, publication
guard lifetime, savepoint ownership, tenant separation, retained history,
checkpoint exclusions, unique keys, SSI row/vault predicates and autocommit
reservation/replay rules. These are volatile state tests in browser workers,
not crash durability or application transaction SDK coverage. On Chromium/Firefox,
the test host caches the conformance worker/WASM and repeats these assertions
after a full offline browser restart with the HTTP listener stopped. The native runtime
transaction, WAL/checkpoint and vacuum suites remain required regression checks.
The state manager retains O(active transactions + retained outcomes/pins + SSI
read sets and conflict edges) memory and adds no IPC or storage operations.
No browser transaction throughput or memory-budget claim follows from this move.

The native entity representation now also lives in `reddb-engine::storage::entity`,
with native aliases for rows/documents, graph nodes/edges, vectors, time-series,
queues, embeddings and cross-references. The canonical binary document-key decoder
feeds the existing bloom algorithm. The host wall clock supplies Unix seconds
(`SystemTime` natively, `Date.now / 1000` in WASM); monotonic query timing is
unchanged. Native entity/value serialization now delegates to the shared codec
below. No field encoding or file version changes in this move.

`SnapshotContext`, raw own/aborted-sub-XID visibility, logical-ID SSI read tracking
and table-row version selection are shared too. `TableRowVersionStore` supplies
the same current/history candidates the native resolver previously consumed;
the generic selector keeps the same filter-before-max-xmin ordering and direct
current-row path. Native dispatch retains auth, tenant, moderation and ambient
statement-frame ownership. Six identical native/WASM cases and a separate JS
wall-clock oracle exercise this seam, including after an offline browser restart.
Native dump/paged reopen tests resolve real persisted rows through the shared
selector and check timestamp/schema/identity compatibility and the moderation gate.
The WASM entity fixtures remain in memory: this does not expose stored-data SQL
or durable application transactions through JS/TS.

Entity relocation adds no payload copy, storage operation or IPC. Version selection
retains O(V) candidate materialization and filtering for V physical versions of
one logical row; current-row lookup retains its native store behavior. SSI sets
continue to key on logical identity, not physical version IDs. No throughput or
browser memory-budget qualification follows from these assertions.

The native entity/value codec is now shared too. `reddb-file::native_value`
owns the persisted value tags, varints, Zstd threshold/level and checked byte
cursor; `reddb-engine::storage::entity_codec` adapts those bytes to the shared
logical entity model. Native dump, paged-record and WAL orchestration retain
thin delegates and their existing metadata/policy code. A frozen corpus from
the original native writer checks all values and eight entities across V1–V12.
Checked lengths, iterative array decoding bounded by the existing JSON depth
limit and verified compressed lengths reject malformed input before unchecked
indexing. No new ordinary value-size policy or full memory qualification is
introduced.

Four identical native/WASM cases cover byte equality, version gates, value and
entity truncation, malformed lengths/tags, nesting and compression. The existing
native/browser/native OPFS fixture also decodes and updates encoded entities,
retains their opaque metadata, restarts entirely offline and verifies the export
natively. Native dump coverage independently retains real native metadata. These
fixtures provide no application schemas, stored-data RQL or transactional entity
publication. Runtime dispatch and durable transaction/WAL integration remain
pending. Codec work is O(encoded bytes), with an array stack bounded by 128
levels and compression/decompression proportional to payload size; this makes
no browser memory or throughput promise.

Native query records/results and entity materialization are now shared too:
`query::unified` owns the schema-shared/overflow representation, system keys,
matched graph records/paths and statistics; `query::records` owns the native
full/lean/projected/universal materializers and typed tag decoding. Native paths
re-export the same types/functions. The stored-graph-node conversion stays in
the native adapter through the standard `From` trait, retaining the existing
`MatchedNode::from_stored` entrypoint without coupling the core to GraphStore.

A frozen pre-extraction native oracle covers 112 results across 14 entity shapes
and eight materialization paths. Six relocated native layout property tests and
four shared native/WASM cases check field/projection/identity behavior. The real
OPFS fixture materializes full/projected records from persisted entities across
an offline restart, and native integration compares actual query output after
dump/paged reopen. Host scan/candidate discovery, schema catalogs, authorization,
tenant/moderation/secret policy and transaction/WAL publication remain in the
runtime; this is not stored-data browser SQL or application transaction support.
Materialization remains O(fields visited + document bytes decoded), with the
existing owned/borrowed copying behavior and no new IPC or storage operations.
No browser memory-budget or throughput qualification follows from the move.

Encryption qualification also requires resolving a pre-existing native header
layout collision: the encryption marker currently occupies the physical-format
version field, and a database-header rewrite overwrites it. The canonical file
codec is unchanged from native 1.25.7. The native encrypted-pager restart
regression is explicitly ignored with this reason and tracked in #2453; it must
pass before encrypted pager restart is claimed. The shared byte-I/O test
separately verifies exact opaque crypto envelopes, authenticated reads after
reopen and rejection of tampered ciphertext. This milestone makes no encryption
format migration or full encrypted-pager qualification claim.

The `executeScalar` entrypoint executes storage-free SELECT
expressions in the worker with the same evaluator and exact JSON integer/decimal
tags. It rejects unsupported clauses explicitly and never reads or writes the
artifact. Operator fixtures run identically as native Rust tests and a separate
test-only WASM artifact inside browser workers. Native runtime differential tests
compare the scalar corpus against actual `RedDBRuntime` results. Query timing
uses the host's monotonic clock (`Instant` natively, `performance.now` in WASM).
The package is released independently as `@reddb-io/browser` on the `latest` channel with
JS, TypeScript declarations, Worker and WASM assets. This artifact/scalar release
is not yet a complete database SDK: storage-backed five-model execution, local
transactions and sync remain pending. See `docs/clients/browser.md` for installation
and frontend integration.

Cost sketch: filtering 10,000 rows visits 10,000 rows; comparison sorting is on
the order of 10,000 × log2(10,000), about 133,000 comparisons. The relocated row
executor retains O(N) materialization and adds no per-row IPC or storage I/O.
This is a portability step, not a throughput or browser memory-budget claim.
Page payload memory remains capacity × 16 KiB (256 resident pages = 4 MiB),
plus cache metadata and existing copies during read/writeback; no new IPC,
disk operation or allocation policy is introduced by sharing these modules.
Protected writeback for 63 pages remains about 3 MiB of writes (63 × 16 KiB
home bytes, the staged frame and the 1 MiB zone clear) and three barriers;
no per-page IPC is introduced. Holding the native file mutex through those existing barriers
serializes the protected batch; it is not a throughput claim.

B-tree cost sketch: lookup retains one page read per tree level plus any overflow
chain reads; a scan visits each leaf and value once. The test corpus uses 600
mixed-width records plus a 100 KB compressed value and a 70 KB overflow value,
then replaces the overflow value with 90 KB. The minimum sharded cache is 32
pages (512 KiB), used by the fixture. Generic page
access adds no per-page IPC or dynamic dispatch, and does not change compression,
allocation, publication or locking policy. This is correctness coverage, not a
browser throughput or memory-budget qualification.

New sync contracts belong to `reddb-wire` (ADR 0046). They require consistent
snapshot/cursor bootstrap, durable resumability, transactional/idempotent upload,
stable cross-device identities and graph references, deletes/retractions, and
normal server-side authorization/RLS. The in-memory CDC notification ring is not
a durable offline sync protocol. Rebootstrap after cursor expiry must preserve
pending intent. Local transactions are atomic locally; offline device writes do
not imply global serializability. Permission revocation reaches an offline
device upon reconnection, not retroactively.

ADR 0047's existing RedUI bridge and remote transport remain unchanged. Any Browser
integration into that UI is a separate SDK/UI change, not a claim that a hosted
page can access a native `file://` path.

## Verification

The foundation requires canonical native/portable file interoperability, failures
at every publication write/flush, published-WAL corruption detection, native
artifact regression tests, and real browser OPFS restart/ownership/corruption
tests on Chromium, Firefox and WebKit. Full Browser additionally requires five-model
query/transaction parity, real Safari/iOS storage checks and two-device sync
under conflicts, duplicates, cursor expiry and permission changes. Bundle size,
startup, memory, storage and latency measurements precede performance claims.


## Resident collection storage

The canonical growing/sealed segment implementation, typed metadata storage,
resident payload accounting, key/unique-key candidate indexes and physical graph
candidate index now belong to `reddb-engine::storage`. Native module paths alias
these types, including primitive metadata conversions and the existing locked
segment trait adapter. This removes the next native dependency from collection
reads and writes without introducing another entity store for the browser.

The native segment manager still owns lifecycle, consolidation, snapshot and
moderation admission; native WAL and catalog publication remain host policy.
Physical candidates provide neither visibility nor authorization. Public browser
stored-data queries and durable application transactions still require the
remaining runtime integration. No file bytes or versions change in this move.

The shared clock supplies segment timestamps in Unix seconds on both hosts.
Portable conformance exercises canonical entity shapes, key-version selection,
raw key mutation, bounded/cancelled scans, metadata and sealing. The browser
conformance artifact runs these same cases, including the offline restart suite.

Data-plane sketch: 10,000 resident entities at 1 KiB each retain about 10 MiB of
payload plus the existing native indexes; aliases add no second resident copy.
Physical scan batches remain bounded at 256 captured slots, as in the native
implementation. This is a size sketch, not a measured performance claim.

## Shared snapshot recovery

The native host and WASM share typed metadata/entity-record encoding and streaming
native V1–V12 snapshot decoding. Physical framing remains in `reddb-file`, whose
borrowed payload verifier checks CRC before restore callbacks without copying the
whole dump. Recovery visits empty collections, one entity/metadata record at a
time, cross-references and the optional V10+ opaque catalog blob. A host must
populate a private recovery target and publish it only after success. Native ID
allocation, derived-index rebuilding, version upgrades and disabled-WAL restore
policy remain native host responsibilities.

The metadata reader validates counts against remaining bytes before allocation
and caps recursion at the existing RQL JSON depth limit. This intentionally
rejects deeper metadata previously accepted by the unbounded native reader.
Frozen original-native metadata bytes and V1–V12 entity fixtures qualify the same
cases natively and in browser WASM. Formats/versions and the public browser API
are unchanged. Application WAL recovery and stored-data dispatch remain pending.
