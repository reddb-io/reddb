# ADR 0077 — RedDB Browser, shared engine and OPFS

Status: accepted architecture; shared native/WASM storage and query core; public read-only table SELECT in source builds; SQL writes, application transactions and sync pending
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

The shared segment manager now owns lifecycle, consolidation, explicit snapshot
and persisted moderation-marker admission; native WAL and catalog publication
remain host policy.
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


## Shared collection lifecycle

`reddb-engine::storage::manager` owns the native collection manager, paced
consolidation, retained-source accounting, snapshot scans, keyed-version probes
and ordered physical graph candidate merging. Native paths re-export the same
types. Host-only integration tests still check ambient snapshot independence,
query-callsite guards and runtime memory admission. The persisted moderation
marker predicate and explicit-snapshot entity gate have one shared authority;
authorization, tenants and moderation providers remain host responsibilities.

Native scans retain their existing scoped-thread and Rayon paths. WASM uses
serial scans and reductions inside the dedicated Worker, without attempting to
spawn Rust threads. The same conformance cases cover reentrant collection gates,
entity shapes, own/aborted savepoint versions, moderation/SSI read tracking,
bounded cancellable scans through consolidation, pinned-source accounting and
ordered graph candidates in native Rust and browser WASM. They are collection
lifecycle coverage; persistent stored-data SQL, catalog integration and durable
transaction publication through the public Browser SDK remain pending.

Cost sketch: scanning N entities still visits N candidates, and consolidation
copies at most the configured entities per tick (4,096 by default). A cursor
retains its original source until completion/cancellation, so an in-flight
consolidation can temporarily retain both source and destination payloads. This
extraction adds no IPC or file-format change and makes no measured speed claim.


## Shared resident read indexes

The native context index, bounded token generation and sharded entity lookup
cache live in `reddb-engine::storage`; native module paths re-export the same
implementations. Native binary document decoding delegates directly to the
canonical `reddb-types` codec. Large unsigned integers retain their existing
canonical tagged JSON representation when generating document tokens.

These indexes return physical candidates. Snapshot visibility, authorization,
tenant scope and moderation admission remain at the runtime read boundary.
Collection enablement controls new context-index writes; disabling it does not
by itself invalidate previously indexed physical candidates. Store mutations
retain their existing index/cache invalidation paths.

Shared native/WASM cases cover binary/tagged-JSON document token equivalence,
token count bounds, collection filters, BM25 ordering, replacement/removal and
cache eviction/batch invalidation. Native integration additionally checks a
binary document inserted and its collection dropped through `UnifiedStore` against the shared
index/cache types. Public browser stored-data queries and durable application
transaction publication still require the remaining store/runtime integration.

Cost sketch: one indexed entity emits at most the existing token/pair bounds;
cache lookup retains the existing per-shard LRU touch of O(C/16) for capacity C.
Relocation adds no second cache or index and no storage/IPC operation. No
measured browser speed or memory-budget claim follows from these tests.

## Shared resident store state and reads

`ResidentStore` owns the canonical live collection map, global entity allocator,
entity/context caches, graph-label and cross-reference maps, and opaque auxiliary
catalog bytes. `UnifiedStore` embeds this state and delegates resident reads and
allocation to it. Native mutation, replay, checkpoint and collection-publication
adapters address the same moved fields under their existing guards; durable
write/publication ordering and paged-read fallback remain in the host.

The host-only fields are internal Rust integration surfaces, hidden from public
API documentation. They expose no new JS/TS capability. Cross-collection raw
maintenance scans still admit every physical version; explicit snapshot scans
retain MVCC and persisted moderation admission through the shared managers.
Neither path supplies authorization or tenancy policy.

Identical native/WASM fixtures cover global identity floors above JavaScript's
exact-number range, captured snapshots across multiple collections/segments,
physical logical-version candidates, all existing entity shapes, cache reads,
graph labels, metadata and reference maps. The real offline OPFS snapshot
fixture reconstructs one resident store and checks collection ownership, empty
collections, cached physical reads and opaque catalog bytes. A native adapter
test verifies manager Arc identity plus allocation, cache and graph-label
invalidation through native mutations. Catalog dispatch, durable mutation
publication and stored-data RQL through the public Browser SDK remain pending.

Cost sketch: a multi-collection scan retains the existing O(collection count)
Arc/name snapshot plus O(candidate count) work; relocation adds no second map,
cache or entity copy. Allocation keeps the same atomic per-ID/block operations.
No file bytes, format versions, WAL record ordering or measured speed claim
change with this extraction.

## Restoring tombstoned physical identities

Logical WAL replay can delete and restore the same physical entity ID. A growing
segment keeps tombstone ordinals for the lifetime of captured scan cursors, so
restoration must retain those tombstones. The shared manager seals a conflicting
growing segment and restores the entity into its successor. Rewrites from sealed
copies use the same rule. Sequence identity and metadata remain unchanged;
existing cursors keep their original membership and subsequent scans see one
restored entity. Shared native/WASM fixtures cover hash/flat segments, repeated
restoration and captured cursors; the native fixture uses encoded WAL actions.

Cost sketch: a conflicting restoration pays one existing segment seal over its
resident entities; fresh identities retain the current insertion path. No
per-row generation metadata, file-format change or extra network operation is
introduced. These recovery tests do not enable browser stored-data queries.

## Shared logical store WAL replay

Typed logical store actions and the storage error types now live in
`reddb-engine`; native paths retain aliases to the same types. The existing
`reddb-file` codec remains the only byte-format authority. Record-list counts
are checked against the remaining length-prefix bytes before allocation, so a
corrupt count cannot request an impossible allocation in WASM. Valid frames
and format versions are unchanged.

The native adapter and resident recovery target use a shared action dispatcher
and resident mutation bodies. Pager/B-tree callbacks retain their original
positions relative to resident deletion, cache invalidation and derived-index
cleanup. Native WAL admission, durable flush, transaction publication and
checkpoint coordination stay in the host. Recovery targets remain private
until successful replay; a failed batch is not a public transaction boundary.

Shared native/WASM fixtures verify frame bytes, truncated actions, bounded
counts, record/metadata restoration, allocator floors, index callback boundaries
and collection replacement with retained old readers. A browser-only fixture
persists encoded actions in a real OPFS artifact, closes the entire browser,
and replays them offline into a fresh resident store. This qualifies logical
recovery primitives, not the pending public storage-backed RQL/transaction API.

Cost sketch: replay of N records retains O(N) decode/index work; collection
replacement temporarily retains its previous and replacement manager while
old readers exist. Moving the code adds no second WAL or IPC per record and
changes no flush ordering. No measured performance gain is claimed.

## Shared native snapshot reconstruction

`ResidentStore::from_snapshot` reconstructs a private resident store through the
existing canonical snapshot visitor. The native file loader delegates to that
implementation and retains format upgrades and host configuration. Collection
creation, identity/row allocator floors, sequence identity, metadata, graph
labels, automatic reference indexing, legacy explicit-reference tails and
opaque catalog bytes follow the existing native restoration path. An error
returns no partially recovered target.

The resident cross-reference mutation also supplies the native pager callback
at its original position: forward/reverse indexes and the source entity change
before persistence, then the registry dirty bit is published. Strict paged
flushing remains in the native adapter. Existing endpoint validation, reference
caps and duplicate handling are unchanged, including the legacy snapshot
loader's handling of optional reference tails.

Identical native/WASM cases load the frozen native snapshot using this
production recovery method, exercise versions 1 through 12, reference policy
and caps, allocator floors, metadata and failed recovery. The real OPFS offline
snapshot restart probe uses the same loader. This supplies resident recovery;
public catalog dispatch and durable application transaction publication remain
pending.

Cost sketch: N snapshot records are decoded and adopted once, with their
existing derived-index work; no serialization round trip or second resident
copy is added. Explicit references retain the existing per-source duplicate
scan and configured cap. No extra file or network operation is introduced.

## Owned artifact publication of resident batches

`EmbeddedResidentStore<F: VfsFile>` combines canonical snapshot recovery, logical
store replay and the single-owner `EmbeddedRdbFile` publication barriers. It
publishes already-admitted logical batches; SQL isolation, catalog admission
and provisional application transaction state remain outside this controller.
Physical reads return owned values and cannot observe a batch while its mutable
publication call is executing. No public JS/TS database capability is enabled
by introducing this controller.

The native snapshot emitter now lives in the shared resident implementation.
The native adapter keeps its publication guard, pending-version/XID projection
and format-version update. A hook retains that update's original position
before the CRC footer while collection/reference guards remain held. Native
frames, deleted-row projection, metadata and auxiliary bytes are unchanged.

Before staging a batch, the owned controller folds the old committed image if
WAL capacity is insufficient. The headroom limit is the same 1 MiB policy used
by native embedded overflow recovery, with larger indivisible actions retaining
their required capacity. Successful batches become readable after the native
WAL and root barriers. Rejected replay restores the committed snapshot/WAL;
ambiguous I/O or interrupted publication fences all reads/writes until reopen.
It cannot checkpoint provisional rows to recover from an uncertain append.

Shared native/WASM cases cover every canonical record shape and metadata,
rejected batches, every recorded WAL/root/checkpoint write and flush boundary,
old-or-new whole-batch recovery and WAL growth with incompressible data. The
OPFS fixture seeds the same corpus, interrupts the WAL flush, restarts the
entire browser offline, restores a deleted physical identity and checkpoints
through the production owned controller. These are durable storage primitives;
public stored-data RQL and application transactions still require runtime work.

Cost sketch: a valid B-byte batch needs O(B) replay/encoding and the existing
WAL/root flush barriers, without a second resident-store copy. Rejected logical
batches reload O(database + live WAL) bytes; overflow checkpoints serialize
O(database) bytes and reserve the existing bounded headroom. No network call or
measured performance gain is introduced.

## Shared collection contracts and schema admission

`reddb-engine::catalog` owns the native collection contract types, runtime/file
codec mappings, immutable contract cache, column-rule resolution and stored
expression validation/dependency ordering. Native paths alias the same types
and functions; native query errors retain their existing messages and category.
`reddb-file` remains the persisted JSON/table byte-format authority. This move
does not change contract defaults, file versions or native metadata publication.

Shared native/WASM cases cover model round trips, legacy defaults, corrupt
contracts, typed table definitions, generated dependencies and invalid CHECKs,
and cache snapshots across replacement, removal and invalidation. A test-only
OPFS fixture persists a canonical resident snapshot containing a collection and
its expression-bearing contract, restarts the entire browser offline with the
HTTP listener stopped, and reconstructs/validates that catalog. Native cache
tests retain their concurrent-load races. The host still owns DDL publication,
mutation enforcement, authorization and transactions. Public stored-data RQL
and application transactions remain pending; browser capability flags stay
unchanged.

Cost sketch: decoding C contracts with K total columns retains O(C + K) codec
work, plus the existing dependency-order scan for generated columns. Updating
an immutable cache with live readers copies O(C) Arc entries; relocating it
adds no second catalog, file operation or IPC. No measured speedup is claimed.

## Shared contract write admission

`reddb-engine::catalog::enforcement` owns the native pure collection-contract
evaluator, row-update descriptors, value coercion, final generated/CHECK
evaluation, batch/candidate uniqueness rules and default-TTL key handling.
Native entry points delegate to these same functions and retain their query
versus invalid-operation error categories and messages. The host supplies the
clock; native time capture remains at its existing call sites.

This is admission over supplied values and candidates. Native locks, indexed
conflict discovery, MVCC key reservation, implicit catalog publication, hooks,
tenant/RLS policy and durable transaction publication remain at their original
boundaries. A caller must retain those protections through publication. No file
codec, format version, isolation level or public browser capability changes.

Shared native/WASM cases exercise schema modes, defaults, coercion, nullability,
generated dependencies and recomputation, CHECK verdicts/evaluation errors,
managed timestamps, unique/primary/null keys, conflict targets, vector dimensions
and TTL overrides. A test-only OPFS fixture recovers admitted generated fields
from WAL after an offline browser restart, rejects invalid updates/conflicts
without appending WAL, publishes a valid update, checkpoints, and restarts
offline again. It materializes and evaluates the recovered values using the
canonical query helpers; it does not provide a public SQL execution facade.

Cost sketch: normalizing C declared columns and F supplied fields retains the
existing O(C + F) temporary values plus coercion/dependency work. The supplied
candidate uniqueness checker retains O(U * E * K) comparisons for U rules,
E candidates and K key columns; the native indexed discovery path is unchanged.
Relocation adds no extra scan, file write or IPC and claims no measured speedup.

## Shared record resolution and WHERE traversal

`reddb-engine::query::runtime` owns the native field/alias/graph binding and
document-path resolution, runtime value comparisons, metadata predicates,
byte-oriented LIKE matching and WHERE traversal. Native entry points delegate
to these same functions. Exact signed/unsigned integer comparison, legacy
identity aliases, binary JSON paths and existing resolution order are retained.
The typed row adapter performs nullable-column catalog lookup only after a
field miss, preserving document and foreign-alias handling.

The host supplies two statically dispatched callbacks: expression evaluation
and secret input admission. The native adapter retains typed evaluation,
fallback only for unknown functions, query-error categories and secret-error
redaction. Visibility, authorization, tenancy/RLS and secret disclosure policy
remain host responsibilities; records must be admitted before filtering.
Existing bool-only entry points retain their error-to-false behavior, while
result-returning paths propagate evaluator errors.

Shared native/WASM cases cover aliases, document paths, graph bindings, lazy
nullable lookup, exact integer comparison, metadata/LIKE predicates, boolean
short-circuiting, expression errors and secret admission. A test-only OPFS
fixture applies parsed RQL WHERE predicates to supplied/recovered records,
restarts the entire browser offline with its HTTP listener stopped, updates
through canonical admission/publication, checkpoints and restarts offline
again. It verifies large integer identities, generated fields, nullable columns,
JSON text/binary paths and arithmetic errors. This fixture is not a public SQL
dispatcher; stored-data queries and application transactions remain disabled
in the public browser capability flags.

Cost sketch: N admitted records retain their existing per-record predicate,
comparison and path-resolution work, with statically dispatched callbacks and
nullable catalog lookup only after a miss. Relocation adds no scans, files,
network traffic or IPC, and claims no measured performance gain.

## Shared runtime ordering and top-K

`reddb-engine::query::runtime::ordering` owns the native ORDER BY comparison,
flat-buffer sort-key extraction, stable full sort and quickselect top-K. Native
entry points delegate to these same algorithms. The host supplies expression
evaluation, secret input admission and identity-key construction; native
expression misses retain their absent-key behavior. The environment-controlled
top-K kill-switch stays in the native adapter and becomes an explicit boolean
returned by a host callback at the shared boundary. Record admission, visibility
and validation remain host responsibilities.

Existing optional-key/NULL and direction semantics, abbreviated text keys,
exact integer comparison and record cloning remain unchanged. Pairwise
comparison retains its identity-key fallback; full sort and top-K retain stable
input-order ties. These are distinct native behaviors, not a new unified tie
policy. Top-K still selects only the `offset + limit` prefix when worthwhile;
the caller applies the actual OFFSET/LIMIT. Bounds that exceed `usize::MAX`
saturate instead of wrapping on wasm32, preventing a wide SQL bound from
silently selecting a small prefix.

Shared native/WASM cases cover signed/unsigned integers beyond JavaScript's
exact range, missing/NULL keys, mixed directions, text-prefix collisions,
expression evaluation once per row/key, stable top-K parity, disabled top-K,
wide bounds, identity/secret callbacks and NaN edge behavior. The existing
test-only OPFS recovery fixture now also evaluates parsed WHERE/ORDER BY queries
with LIMIT/OFFSET over recovered generated fields, nullable columns and JSON
paths, before/after an update and checkpoint, with full offline browser
restarts and the HTTP host stopped. This composition over supplied records is
not a public stored-query dispatcher; browser stored-query and application
transaction capability flags remain false.

Cost sketch: N records and K ordering clauses retain O(N * K) key extraction
and key storage, O(N log N) full-sort comparisons or average O(N) selection plus
O(L log L) prefix sorting for L = offset + limit, and the existing output
record clones. The move adds no scan, file write, network traffic or IPC; no
measured speedup is claimed.

## Shared runtime projection and sensitivity traversal

`reddb-engine::query::runtime::projection` owns the native SELECT projection
traversal, output-label and visible-column handling, legacy identity aliases,
document/entity projection gates, materialized window reads and conservative
secret-sensitivity walks. Native entry points call these same functions. Graph,
path and vector bindings retain their existing clone/passthrough behavior.
SELECT * keeps secret wrappers intact for downstream native output policy;
explicit sensitive projections still pass through the host's output check.

The host supplies typed evaluation, legacy function/filter fallback, legacy
literal decoding, error conversion/redaction and secret output policy. Unknown
function/column errors retain the existing projection fallbacks, while genuine
typed-evaluator data errors propagate. This deliberately retains the native
distinction between typed expression evaluation and legacy projection scalars,
including arithmetic lowered to legacy function names. Those native scalar
dispatchers and crypto/session policy are not reimplemented by this extraction.
Sensitivity still includes untaken CASE branches and window expressions.
Visibility, authorization, tenancy/RLS and final serialization remain host
responsibilities.

Shared native/WASM cases cover field/identity aliases, host literal delegation,
columnar and overflow layouts, star visibility, duplicate output labels, binary
document paths, graph/vector bindings, materialized windows, typed scalar and
boolean results, fallback categories, data-error propagation, CASE sensitivity,
output rejection and secret-error redaction. The existing offline OPFS fixture
now also checks projected rows and output columns after filtering, ordering and
pagination across WAL recovery, an admitted update, checkpoint and full browser
restarts with the HTTP listener stopped. Its test host uses canonical typed
evaluation for supported fixture expressions; it is not a complete browser
scalar host or public stored-query dispatcher. Public stored-query and
application-transaction capabilities remain false.

Cost sketch: N rows with P projections retain O(N * P) projection visits plus
existing expression/path work and output-value/binding clones. Visible-column
collection retains native schema sampling and heterogeneous-layout fallback;
sensitivity keeps its existing AST traversal. The move adds no storage scan,
file write, network traffic or IPC, and claims no measured speedup.

## Shared native projection numeric scalars

`reddb-engine::query::runtime::numeric` owns the native projection dispatcher’s
numeric-operand representation/coercion, arithmetic operations, millisecond
timestamp arithmetic and CAST helper. Native entry points alias these exact
functions/types. Argument resolution, legacy literal decoding, session/secret
policy and the remaining scalar dispatcher stay in their existing native host.

This preserves the existing distinction from typed expression evaluation:
legacy projection division/modulo by zero returns NULL, numeric division
returns Float, numeric text/decimal coercion follows the native dispatcher,
and millisecond timestamp operations retain checked arithmetic. Existing
f64 intermediate arithmetic, unsigned-to-signed coercion and saturating casts
are characterized rather than silently changed during extraction; this is not
a new exact-integer arithmetic guarantee. Typed evaluator/comparison semantics
are unchanged.

Shared native/WASM cases cover operand coercion, result types, NULL/zero and
invalid inputs, timestamp bounds, existing precision behavior, CAST fast paths
and validator fallback. The offline OPFS fixture additionally projects actual
MUL/DIV fallback results over recovered rows after WHERE, ORDER BY and pagination,
including NULL for a zero divisor, before/after an admitted update and checkpoint.
Its test host resolves supported simple arguments through canonical expressions;
it is not a complete legacy scalar host or public stored-query dispatcher.
Stored-query and application-transaction capability flags remain false.

Cost sketch: each arithmetic/CAST call retains its existing constant-size
operand work plus O(B) parsing/coercion when a B-byte text value is supplied.
The move adds no record copy, storage scan, file write, network traffic or IPC,
and claims no measured speedup.


## Shared native scalar argument resolution

`reddb-engine::query::runtime::projection_value` owns the native optional scalar
argument traversal and legacy `LIT:` / `@RL:` decoding. The native adapter
supplies typed evaluation without database/table/alias context, legacy
function/filter effects and ambient secret input admission. Database-aware
scalar dispatch remains in the native host. Optional arguments retain fallback
on any typed evaluation error; top-level projection still propagates typed data
errors and falls back only for unknown functions/columns. These intentionally
different native policies must not be conflated.

Direct literals retain their early return before input admission. Aliases still
admit the resolved inner value and then the outer value; denied inner values
skip the outer check. Fields and materialized window outputs retain their native
lookup semantics. The composite decoder retains its legacy text fallback and
quoted-comma/bracket quirks, rather than substituting JSON semantics. Incomplete composites are rejected instead of recursively retrying the same
payload. Nesting beyond the canonical RQL JSON depth cap (root depth zero) is
also rejected; both follow the existing undecodable-composite text fallback.

Native/WASM cases cover nested arithmetic arguments, literals, malformed
composites, the depth boundary, field/alias/window resolution, data-error fallback
and input admission ordering. The test-only offline OPFS fixture now executes
nested `(quantity + 1) * 2` through these same argument and arithmetic functions
over recovered rows, before/after update and checkpoint with the HTTP listener
stopped. Its remaining scalar effects are explicit test sentinels, not a complete
browser scalar host. Public stored-query and application-transaction capability
flags remain false.

Cost sketch: P argument AST nodes retain O(P) traversal plus typed evaluation,
existing value clones and B-byte literal parsing; nested legacy arrays retain
repeated substring scans bounded by depth 128. The extraction adds no storage
scan/write, network traffic or IPC and claims no measured speedup.


## Shared native common scalar dispatcher

`reddb-engine::query::runtime::scalar_functions` owns the legacy projection
arms for arithmetic, CASE, CAST, COALESCE, concatenation, case conversion,
character/byte/bit lengths, reversal, ABS/ROUND and MONEY construction/accessors.
The server delegates to those same arms. The host supplies optional argument
resolution with input admission, database-free CASE conditions and the remaining
specialized/effectful fallback. Native database-aware dispatch still runs before
this shared layer. No new dependency or alternative JavaScript evaluator is added.

Legacy names stay case-sensitive after stripping an output alias suffix.
Missing arguments retain None independently of SQL NULL. CASE and COALESCE
retain branch/argument short-circuiting; CONCAT variants retain their distinct
NULL handling and plain-text/display-string conversions. MONEY retains its
one-versus-two-argument error behavior and reversed retry/admission order.
Native numeric, typed-evaluator and output-secret policies are unchanged.
Clock/session/crypto/geo, regex and remaining text functions stay in the host.

Shared native/WASM cases exercise results, Unicode lengths, malformed/missing
arguments, fallback without eager resolution, branch short-circuiting, money
retry order and input denial. The offline OPFS fixture now projects CASE over
recovered quantities and concatenates nullable names with CAST values across
WAL recovery, an update and checkpoint, with full browser restarts and the HTTP
listener stopped. This test-only host remains partial. Public stored-query and
application-transaction capability flags remain false.

Cost sketch: N rows retain their existing per-call argument/condition visits;
CASE/COALESCE skip unselected arguments, text operations scan their existing B
bytes/characters and allocate their existing output strings, and MONEY performs
at most two coercions. The move adds no storage scan/write, network call or IPC
and claims no measured speedup.


## Shared native text scalars and wide bounds

`reddb-engine::query::runtime::text` owns the native Unicode substring,
regex substring, position, left/right slice, trim and literal-quoting helpers.
Both the typed native evaluator and legacy projection dispatcher use them.
The shared scalar dispatcher now owns SUBSTRING/SUBSTR, POSITION, TRIM/BTRIM,
LTRIM/RTRIM, LEFT/RIGHT and QUOTE_LITERAL. Argument admission, missing-versus-NULL
behavior and typed-to-legacy fallback policy are unchanged.

The engine explicitly depends on the same `regex = "1"` implementation already
used by the server; standalone driver locks record that dependency edge without
upgrading packages. Regex first-capture behavior, invalid/no-match NULL handling
and the existing parser depth limit of 250 are preserved. The limit is explicit
in the builder; regex compilation and allocation defaults remain unchanged.
No JavaScript regex replacement or pattern translation is added.

Wide substring start/count and negative slice lengths saturate rather than
truncate on wasm32. This preserves native 64-bit results for bounds above
2^32 and i64 extremes: an out-of-range start returns empty text, a large positive
count takes the remaining text, and a large negative LEFT/RIGHT length removes
all characters. Existing negative substring-count rejection and optional
missing/NULL argument quirks remain characterized rather than normalized.

Shared native/WASM cases cover Unicode, wide bounds, regex captures and the
regex depth boundary, trims/quoting, missing/NULL arguments and input denial.
The offline OPFS fixture now projects wide substrings, regex captures, slices
and quoted values over recovered nullable fields before/after WAL recovery,
update and checkpoint, with full browser restarts and the HTTP listener stopped.
Native typed-evaluator regressions also cover these shared helper bindings.
Public stored-query and application-transaction capabilities remain false.

Cost sketch: B-byte strings retain their existing scans/output allocations and
substring/slice character buffers; regex calls retain per-call compilation
bounded by the existing parser/program limits. Sharing removes duplicate helper
bodies without adding a scan, storage write, network call or IPC. No measured
speedup or production WASM size reduction is claimed.


## Shared temporal scalars and host wall clock

`reddb-engine::query::runtime::temporal` owns native time-bucket conversion,
bucket rounding and UTC time helpers. The common scalar dispatcher now owns
TIME_BUCKET, NOW/CURRENT_TIMESTAMP, CURRENT_DATE and CURRENT_TIME. Its host
supplies Unix milliseconds; the native adapter uses SystemTime with the same
error/pre-epoch zero fallback, while the shared WASM clock uses the browser's
existing Date.now import. Native typed evaluation delegates to the same helpers.
Clock calls retain one fresh read each, ignored extra arguments and existing
UTC date truncation/time remainder behavior; no query-wide clock freeze is added.

Typed duration arguments retain numeric nanoseconds or canonical duration text.
Legacy projection duration arguments still require a LIT sentinel and do not
consume input admission. Legacy timestamp resolution retains its distinct
behavior: a missing/unresolved/denied explicit argument falls back to raw
admitted-source timestamp_ns/timestamp_ms/timestamp fields; a resolved NULL or
invalid value does not. Field names do not infer units. Zero-width/raw buckets
still return the original timestamp. Session/secret policy is not moved here.

Two integer-overflow paths now reject out-of-range input instead of panicking
in debug or wrapping in release: compact duration multiplication (now checked,
like long-form durations) and seconds/milliseconds converted to nanoseconds.
Valid native conversions and existing float coercion are unchanged.

Native/WASM conformance covers duration units, wide integer buckets, conversion
bounds, missing versus NULL arguments, fallback order, deterministic clock values
and ignored arguments. A real Worker compares shared clock scalar results with
Date.now before/after the WASM call. The offline OPFS fixture projects time buckets
and fixed-clock date/time results after WAL recovery, update and checkpoint with
full browser restarts and the HTTP listener stopped. Public stored queries and
application transactions remain pending and their capability flags remain false.

Cost sketch: each temporal scalar retains constant-size conversion/arithmetic,
O(B) parsing for a B-byte duration and at most three raw fallback field probes;
clock scalars perform one host clock read. No scan, storage write, network call
or new dependency is added, and no measured speedup is claimed.


## Shared geographic computation and projection scalars

`reddb-engine::geo` owns the existing native geographic recognition, WGS-84 and
spherical distance/bearing/midpoint/destination helpers, bounding boxes and
polygon algorithms. The native `geo` module re-exports these same implementations;
its H3 child remains native. Existing native property/unit tests move with the
pure code, while portable native/WASM assertions exercise the same algorithms.
No new dependency or JavaScript geographic implementation is introduced.

The common scalar dispatcher owns GEO_DISTANCE/HAVERSINE,
GEO_DISTANCE_VINCENTY/VINCENTY, GEO_BEARING and GEO_MIDPOINT. Shared legacy
argument resolution retains 2/3/4-argument forms, POINT sentinel parsing,
column/document lookup, legacy longitude-sibling priority and existing numeric
coercion. Direct column/sentinel reads retain native admission behavior; other
projections and numeric arguments use the host resolver. Raw secret wrappers
are not recognized as coordinates. No new point validation policy is imposed.
The native H3_CELL wrapper delegates argument resolution to this same helper.

GeoJSON coordinate order, exact decimal coordinate recognition, range checks,
polygon boundary inclusion and bounded Vincenty iteration/fallback are preserved.
Geographic JSON uses the existing canonical parser and its depth-128 cap.
Comparisons in browser fixtures allow floating-point tolerances appropriate to
native/WASM transcendental implementations; no bitwise float parity is claimed.

The offline OPFS fixture now projects distances, bearings and midpoints from
recovered quantities after filtering/ordering, WAL recovery, update and checkpoint,
with full browser restarts and the HTTP listener stopped. Its scalar host is
still partial; public stored-query and application-transaction flags stay false.

Cost sketch: each distance/bearing call keeps constant-size numeric work;
Vincenty retains at most 100 iterations, JSON point recognition parses its
existing B-byte input and polygon operations remain O(V) in vertices. The move
adds no storage scan/write, network call or IPC; no speedup or size gain is claimed.


## Shared canonical table-plan traversal

`reddb-engine::query::logical_plan` owns the existing CanonicalLogicalNode and
CanonicalLogicalPlan data types; the native planner retains re-export shims and
its existing construction/cost/access-path logic. The shared table-plan runtime
walks the same scan/filter/document-filter/sort/offset/limit/search/top-K/projection
operator names used by the native canonical executor. The native executor now
delegates to that walk rather than maintaining another operator dispatcher.

The host retains physical scans and entity-ID shortcuts, MVCC/replica visibility,
tenancy/RLS and document-root admission, resource guards, computed ordering keys,
ranking, window evaluation and output policy. Filter lookup still precedes child
execution and may bypass a missing child; document filters never take that shortcut.
Callbacks preserve their native ordering, first-child behavior, errors and effects.
Unsupported/missing-child diagnostics keep their native text and query-error kind.

Visited plan depth is bounded by the canonical RQL nesting cap (root at zero).
OFFSET/LIMIT and explicit top-K bounds saturate on wasm32 rather than truncating
or falling back to a smaller query limit. Native scan-cap pushdown uses the same
shared saturated offset-plus-limit helper, preventing an overflowing sum while
retaining the rule that ORDER BY scans cannot push down a finite row cap.

Native/WASM cases cover callback order, operator aliases, fast lookup, empty
results, error propagation, wide bounds/top-K and depth boundaries. The offline
OPFS fixture additionally runs a fixed canonical plan through shared filtering,
ordering and projection over recovered rows after WAL recovery, update and
checkpoint with full browser restarts and the HTTP listener stopped. The fixture
host and fixed plan are test-only; there is no new browser planner or complete
public stored-query dispatcher here. Stored-query/transaction flags remain false.

Cost sketch: D visited operators retain O(D) dispatch plus existing host scan,
filter, sort, projection/window work and record buffers. The visited stack is
bounded at depth 128; no new scan, file write, network request or IPC is added.
The extraction makes no measured performance claim.

## Shared canonical table-plan construction

`reddb-engine::query::planning` now constructs the native canonical table plan
from the canonical TableQuery AST. The native planner delegates its table arm to
this implementation. AccessPathDecision and CardinalityEstimate retain their
native import paths through re-exports of the shared types. The host still owns
index availability/lifecycle, catalog document admission and live/statistical
cardinality decisions; no native optimizer, index selection or RLS rule is replaced.

The same construction preserves scan metadata, filter/sort/offset/limit/projection
order, universal entity access and implicit ranked sorting, document operator
precedence, effective SQL projections/predicates, row estimates and operator costs.
Shared metadata helpers keep their existing summaries, including secret/password
redaction. The recursive predicate summary is capped at the canonical RQL nesting
limit; deeper descriptive metadata uses `<maximum filter depth>` without changing
the query or its execution predicates.

The offline OPFS test host now supplies recovered rows and scan statistics to this
canonical planner and executes its generated tree through the shared table runtime,
replacing the hand-built operator chain. Native/WASM cases verify metadata, host
callback ordering, estimates/costs, document/universal routes, zero pagination and
the metadata depth boundary. Native integration additionally checks the live
collection count and SQL results. The fixture remains test-only: the public browser
stored-query/transaction capabilities remain false until the complete production
host, dispatch and durable transaction boundary are wired and qualified.

Cost sketch: construction emits at most six table operators plus existing AST
summary work; predicate summary recursion is capped at depth 128. No per-row
planning, additional scan, file write, network request or IPC is introduced, and
no measured speedup is claimed.

## Shared runtime expression traversal

`reddb-engine::query::runtime::expression` now owns the native optional-value
Expr walk used by ORDER BY and the existing runtime expression fallback. This
is the legacy runtime contract, distinct from the typed scalar evaluator:
missing values remain `None`, legacy arithmetic/coercion/casts retain their
behavior, boolean/CASE branches short-circuit, and ordinary function arguments
are evaluated eagerly from left to right with misses supplied as SQL NULL.

The native adapter retains record bindings, secret input policy, value-function
dispatch, session identity, advisory locks, catalog/config/KV authorization and
all database/AI/model effects. CONFIG/KV/internal reference calls receive raw AST
arguments before ordinary argument evaluation. Evaluated special defaults use
the callback supplied by the shared walk so they cannot reset its depth counter;
bare-column defaults retain the existing native path-text interpretation.

Visited expression depth is capped at the canonical RQL nesting limit, returning
`None` before host effects on an over-depth node. Unvisited lazy branches remain
unvisited. Negation of the signed minimum Integer/BigInt now returns `None`
instead of panicking or wrapping. Existing optional-key ordering/error admission
remains in the host; this does not replace the typed evaluator's diagnostic API.

Native/WASM cases cover lazy branches, eager function callbacks, raw special
arguments, missing values, casts, exact integer comparisons, secret admission,
unsupported nodes, checked negation and depth boundaries including special
defaults. The recovered-row ordering fixture now uses this same Expr walk; its
function host remains test-only and deliberately supports only its COALESCE
callback. Its generated canonical plan orders using CASE, negation and CAST after
WAL recovery, update, checkpoint and a full offline browser restart. Native tests
also exercise special-default semantics and that SQL ordering path.

Public browser stored-query/transaction flags remain false. The production
browser host and complete durable SQL dispatch are still pending; no new public
JS/TS query surface or persistence format is introduced by this extraction.

Cost sketch: E visited expressions keep O(E) traversal plus existing argument
buffers and host work, with stack depth capped at 128. The move adds no scan,
file write, network request or IPC and makes no measured speedup claim.

## Shared resolved-value scalar dispatch

`reddb-engine::query::runtime::value_functions` now owns the native builtin
function dispatcher over already-resolved Value arguments. The native Expr host
and its existing scalar shim delegate to this implementation; the recovered-row
ordering fixture also uses it instead of its handwritten COALESCE callback.
Existing missing/NULL behavior, case-sensitive dispatcher names, value coercion,
JSON result representations and text/time/money/geo algorithms are retained.

The host supplies Unix milliseconds and retains session identity/tenancy,
advisory locks, H3, password verification and unknown functions. Native model,
AI, configuration/KV authorization and raw special references remain in their
existing expression callbacks. No crypto/auth/server dependency enters WASM.
The shared JSON helpers decode binary document bodies through the canonical
keystone codec; no alternate JSON/storage format is introduced.

Recursive CONTAINS walks and JSON paths/updates use the canonical RQL depth cap.
JSON path indices parse as u64 and convert explicitly at array access, so a
valid out-of-range read is SQL NULL on native and wasm32. JSON_SET rejects
oversized growth before reserving memory or adding one to an unchecked index.
One call shares a padding budget of JSON_LITERAL_MAX_BYTES / 5 new array slots,
using the existing 16 MiB literal-byte budget and the serialized ",null" cost;
nested arrays cannot reset that budget. Reservations are fallible and exact.
Rejected growth returns SQL NULL without mutating the input. Too-deep or invalid
paths keep the existing unresolved-result convention. Integer ABS at the signed
minimum now returns None instead of panicking/wrapping; bit lengths multiply
a checked i64 count rather than a target-width usize.

Native/WASM cases cover pure function results, missing arguments, Unicode and
wide text bounds, deterministic clock values, money and geo inputs, binary JSON
reads, JSON construction/update, depth/wide-index bounds and host fallback.
The recovered canonical plan additionally calls JSON_EXTRACT_TEXT on the binary
body in its CASE sort key after WAL recovery, update, checkpoint and a full
offline browser restart. Native SQL composes JSON_OBJECT/extraction/casts through
the same dispatch. The fixture still rejects native-only effects; public browser
stored-query/transaction flags remain false pending complete production wiring.

Cost sketch: value dispatch is constant-size selection plus existing argument,
text/JSON and geometry work; JSON_SET growth is O(new slots) under one bounded
padding budget and recursion is capped at 128. No new scan, persistence write,
network request or IPC is added. No measured speedup is claimed.

## Shared window phase

`reddb-engine::query::runtime::window_phase` owns the active native algorithm for
ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD and SUM/COUNT/AVG/MIN/MAX OVER. The native
adapter delegates to it and keeps materialization admission, typed expression
execution with secret-aware inputs, the existing db-less legacy fallback, error
conversion and secret output policy. The shared phase retains canonical keys,
stable partition ordering, native Value variants, supported frame diagnostics,
per-column publication and cross-row secret sensitivity. Its position in the
native table plan is unchanged, including existing LIMIT/OFFSET placement.

LAG/LEAD offsets remain nonnegative u64 until checked index arithmetic; a target
outside the partition uses the default on native and wasm32. ROWS PRECEDING
saturates offsets beyond usize instead of truncating on wasm32. Lowered constant
subtraction is checked. Projection argument recursion is capped at the canonical
128-level limit before sensitivity/lowering; embedded Expr/Filter trees and
window specs retain their parser/AST admission limits. No new query parser,
JavaScript evaluator or storage format is introduced.

Eight native/WASM case groups cover partitions/ties, retained rich values,
aggregate peers/frames, wide offsets, errors/publication, admission and cross-row
secret policy. The recovered table-plan fixture exercises ROW_NUMBER, LAG and
SUM OVER after WAL recovery, update, checkpoint and an offline OPFS restart.
Public browser storedQueries and transactions remain false until production
catalog/admission/SQL dispatch and complete durable transaction wiring land.

Cost sketch: for N rows and K key dimensions, each window keeps O(N*K) key memory
plus O(N) indices/results and O(N log N) sorting; existing aggregate frame walks
can cost O(N^2). The extraction adds no network request, disk write or IPC and
makes no measured speedup claim.

## Shared aggregate kernels

`reddb-engine::query::runtime::aggregation` now owns the native slot compiler,
accumulators, batch merging, typed group keys, numeric coercion and result
construction for COUNT, SUM, AVG, MIN, MAX, VARIANCE, STDDEV, MEDIAN, PERCENTILE,
GROUP_CONCAT, STRING_AGG, FIRST, LAST, ARRAY_AGG and COUNT_DISTINCT. Native generic
aggregation delegates to these kernels. Its fast routes reuse the same numeric
coercion and sum conversion. The host retains value comparison/secret policy and
static projection evaluation; scan/visibility, grouping orchestration, HAVING,
ORDER BY, limits, authorization and spill I/O remain native responsibilities.
The native spill codec and field order are unchanged; shared state fields remain
available to that codec and are not exposed as public JS/TS bindings.

A regression test reproduced a native shared-slot bug: SUM/AVG/VARIANCE/STDDEV on
10 and 20 accumulated the source four times, returning SUM 120 instead of 30.
The compiler now assigns one updating projection per storage slot and marks the
other projections as shared reads. A later statistical projection upgrades the
updating slot to collect squared values, so projection order cannot change the
result. The same rule avoids duplicate collection for MEDIAN/PERCENTILE and
GROUP_CONCAT/STRING_AGG. Output labels and parameter evaluation remain unchanged.

Rendered labels cannot identify an argument: a field and a literal can render
alike. Slot identity therefore uses the complete typed argument's derived Debug
rendering, including source spans, as process-local compiler metadata. It is
never persisted or returned as an output name; no storage/wire format depends on
it. This distinguishes argument shapes and remains reflexive for NaN literals.
Projection recursion is capped at the canonical 128-level limit before rendering
or cloning. Embedded Expr/Filter trees retain existing parser/AST admission.

Eight native/WASM case groups cover shared slots, NULL/empty input, rich ordered
values, aggregate parameters/aliases and typed argument identity, merging,
numeric/nonfinite coercion, exact group keys, depth and host callbacks. Native SQL
regression tests force the generic aggregate route in both projection orders.
The recovered fixture runs filtered COUNT/SUM/AVG/MEDIAN grouped by a JSON tenant
field after WAL recovery, updates, checkpoint and offline OPFS restarts. These
are test hosts: public storedQueries/transactions flags remain false pending
production catalog/admission/dispatch and durable transaction integration.

Cost sketch: N rows update U distinct admitted argument/storage slots in O(N*U),
without re-evaluating shared projections; compiler metadata is O(total argument
AST size). Existing list/statistic aggregates retain O(N) values per group and
sort on result construction; host memory/spill policy remains required. No extra
network request, persistent write or IPC is added; no measured speedup is claimed.

## Shared aggregate reference planning

`reddb-engine::query::planning::aggregation` owns the active native preparation
of aggregate references in HAVING and ORDER BY. It reuses visible output aliases,
registers hidden aggregates, rewrites nested expression references and deduplicates
the complete aggregate projection list before slot compilation. The native
executor delegates to this plan; its grouping/scan, H3 admission, authorization,
secret policies, spill, result construction, pagination and resource admission
remain in the native host. Legacy signature rendering and canonical/legacy AST
field precedence are retained.

A new regression reproduced hidden-result label aliasing: PERCENTILE(x, 0.25)
in HAVING and PERCENTILE(x, 0.75) in ORDER BY both wrote `percentile(x)`, causing
otherwise qualifying groups to disappear. Hidden results now keep the existing
label when available and receive distinct private aliases on collision. The
allocator reserves visible labels, source-column/group labels and previously
registered hidden labels; user aliases that already resemble generated names
are skipped. Native visible result columns are unchanged. Colliding hidden
materialization names are intentionally different; no storage/wire format or
public JS/TS binding depends on this compiler metadata.

An iterative guard bounds the planning paths across Expr, Filter, Projection,
window keys and frame expressions before recursive lowering/rendering/cloning.
Lowered projections/filters and newly materialized hidden arguments are checked
again to catch expansion. Native admission invokes the source guard before its
own recursive shape/H3 work. Limits use the canonical 128-level RQL constant;
opaque subquery bodies, literal-container contents and source resolution retain
the host's existing parser/value admission bounds. Oversized planning trees fail
with a query error, without publishing a partially prepared plan.

Six native/WASM case groups cover aliases and hidden reuse, legacy AST authority,
projection deduplication, CASE/casts/predicates, aggregate parameters, pagination,
label collisions, signatures, unsupported outer functions and depth bounds.
Native SQL covers alias reuse and hidden references as well as parameter/alias
collisions. The recovered-row fixture now compiles the shared plan and evaluates
hidden MAX references in HAVING/ORDER BY with LIMIT/OFFSET after WAL recovery,
updates, checkpoint and an offline OPFS restart. Public storedQueries and
transactions remain false until production SQL/catalog/admission and durable
transaction integration are wired.

Cost sketch: for E visited AST nodes and A aggregate references, planning retains
O(E) AST/metadata space plus the existing rendered signatures; nested signature
construction retains its existing bounded rendering cost. New label reservation
uses O(A + visible/group labels) metadata and adds no per-row work, persistent
write, network call or IPC. No measured speedup is claimed.

## Shared aggregate result assembly

`reddb-engine::query::runtime::aggregate_results` now owns the active native
post-group result phase: group/aggregate columns, hidden values, empty-input
behavior, HAVING, sort dispatch, pagination and sensitivity traversal before
secret output. The native generic executor delegates to it. The host retains
materialization limits, group label/key policy, typed/legacy HAVING and ORDER BY
effects, comparison/static-expression callbacks and secret output policy. The
phase consumes borrowed group views and prepared metadata; callers must supply
admitted query trees and states belonging to the same compiled slot plan.

The original ordering is retained: guard the full merged group cardinality,
materialize/evaluate HAVING, sort, apply OFFSET/LIMIT, then protect secret output
for retained rows. Errors propagate without conversion or a partial returned
result. Hidden values stay outside the visible column vector. Native column
ordering and empty grouped schemas are preserved, as is the scalar empty-input
row (COUNT 0, other aggregates using their existing NULL rules), including its
HAVING evaluation. No new SQL admission, spill or persistence format is added.

LIMIT/OFFSET now use checked target-width conversion with saturation instead of
`as usize`. A 2^32 limit must retain a small result on wasm32, while a 2^32 offset
must exhaust it. Native and WASM cases cover those bounds, i64::MAX literals and
u64::MAX AST metadata; the SQL literal grammar is unchanged. Four case groups
cover grouped/hidden columns, empty-input semantics, pagination and resource/
secret callback order and failures. Native SQL adds empty and wide-page
regressions; existing native secret/transaction/persistence checks remain gates.

The recovered OPFS fixture replaces its manual output-row construction and
post-group processing with the shared phase, using actual recovered group
values/states after WAL recovery, updates, checkpoint and offline restart.
The fixture's host admits its fixed tenant labels and pure predicates only;
production browser catalog/admission and query/transaction dispatch remain
pending, with public storedQueries/transactions flags still false.

Cost sketch: G groups with P projections retain the existing O(G*P) result walk
plus existing finalization/sort costs and result memory. Borrowed group views
add no aggregate-buffer copies. No extra scan, persistent write, network call
or IPC is introduced; no measured speedup is claimed.

## Shared aggregate group bindings and shape validation

`reddb-engine::query::planning::aggregation` owns native GROUP BY alias
resolution, structural projection validation and prepared group bindings. Each
binding retains the resolved expression, raw record key and visible output
label. Generic native grouping and shared result assembly reuse these bindings;
result construction and secret sensitivity traversal no longer call the host to
lower projections or render group names per output group. The original native
helpers moved into the engine; there is no separate JavaScript implementation.

Only unqualified group names resolve SELECT aliases, with the existing
case-insensitive match. Qualified fields, canonical/legacy AST precedence,
function/JSON keys, TIME_BUCKET rendering and structural diagnostics retain
native behavior. Source, lowered and alias-resolved trees are depth-checked
before recursive rendering. Native H3 admission remains a host callback, in its
original projection-before-group order; expression effects, resource admission,
secrets, tenancy and persistence stay with their existing hosts. No H3 runtime
or dependency is added to WASM.

Five native/WASM case groups cover aliases, qualified names, expression labels,
legacy metadata, depth limits, structural/host errors and multiple result
bindings. Native SQL regressions force the generic path for alias/function
binding and check H3 admission. The recovered OPFS fixture now groups by the
SELECT alias of its JSON tenant expression and consumes canonical labels without
hardcoded host naming callbacks. These fixtures do not publish SQL/catalog or
durable transactions through the public browser SDK; storedQueries and
transactions remain false.

Cost sketch: for G groups, K group expressions and P projections, group naming
moves its projection matching/rendering from O(G*K*P) to query preparation
O(K*P), retaining O(K) bindings plus resolved expression trees; result insertion
remains O(G*K). Structural admission and execution each prepare metadata once.
There is no added network request, disk write or IPC, and no measured speedup
is claimed.

## Public read-only table SELECT

Source JS/TS builds expose `openDatabase({ name, worker? })` and `query(sql)` over
an existing OPFS artifact. `BrowserDatabase` delegates to
`reddb-engine::query::local_read::LocalReadDatabase`, which owns the canonical
`EmbeddedResidentStore` recovery controller and decodes its authoritative
collection-contract blob once. Snapshot and committed WAL replay, entity scan
visibility, record materialization, unary table planning/execution, projection,
ordering and scalar evaluation retain their existing Rust authorities. There
is no JavaScript evaluator, alternate snapshot or WAL codec. Native server
hosts retain their current transaction, tenancy/RLS and secret policies.

A canonical-expression callback in shared projection defaults to the original
native lowering/fallback behavior. The local host supplies its admitted original
SELECT expressions by projection index. This avoids reconstructing arithmetic
as legacy MUL/ADD function calls while preserving checked evaluator failures.
No native host opts into different evaluation behavior as part of this change.

The first public stored-query surface is read-only, local file-owner access to
one table: scalar projections, aliases/JSON paths, WHERE, source-expression
ORDER BY, and literal LIMIT/OFFSET. Admission checks query shape and every
expression before scanning, including empty tables. Unsupported statements,
functions, parameters, grouping/windows/joins, DISTINCT, system/universal
sources, other models and TTL/retention fail explicitly. Missing or corrupt
catalogs are never inferred. Visible source materialization is bounded at
100,000 rows independently of SQL LIMIT. Native committed scans hide superseded
and moderated rows. Secret expressions are rejected; wildcard secret output
retains canonical redaction. This API is not a server principal/tenant session:
applications must export authorized subsets before offline file-owner access.

Database and raw artifact handles share the exact same Web Lock and exclusive
OPFS sync-handle lifetime. A missing database is not created; read-only open,
SELECT and failed writes never append, checkpoint or alter file bytes. Mutation
and transaction APIs are absent. Engine metadata reports storedQueries true
with storedQueryMode read-only-tables; transactions and sync remain false.
Storage-independent and foreign-language bridges retain their scalar-only
operation lists. The published 0.1.0 package predates this source API; publishing
another browser version is a separate release step.

Native tests cover snapshot/WAL update/delete recovery, moderation/history,
nullable and JSON fields, exact integers, data errors at each query phase,
wide pages, unsupported empty-table queries and authoritative catalog errors.
Real browser and installed TypeScript/Vite consumers import a native artifact
and query it through production bindings after a full offline browser restart,
with the HTTP listener stopped. Read-only bytes and exclusion of raw/SQL owners
are checked through the public SDK; no test-only executor supplies the results.

Cost sketch: open reconstructs O(snapshot + committed WAL) bytes once; each query
walks physical source rows until exhaustion or its 100,001st visible candidate, retains
O(N * row size + projection/order keys) memory and uses existing O(N log N) sort.
Recovered resident state remains in the owning worker between queries. There
is one worker request/response per query and no query-time file write, flush,
network call or measured performance claim.

The dependency audit for this integration exposed existing zstd advisories,
including RUSTSEC-2026-0410 in the WebAssembly allocator shim. Direct RedDB
compression users now require zstd 0.14.3 with zstd-safe 8.2.0 and zstd-sys
2.1.2; workspace and standalone Node/Python locks retain that resolution.
The bundled Zstandard library remains 1.5.7, with existing native
compressed-frame/value/entity fixtures serving as compatibility gates.

Tonic 0.14.6 still requires zstd 0.13 and therefore keeps a second legacy
binding in native gRPC graphs. It is absent from the WASM dependency graph.
The workspace/driver audits remain blocked by RUSTSEC-2026-0391, 0398 and 0403
plus unsafe-API warnings on that transitive binding. The audit configuration
and gRPC compression contract are unchanged. Landing this integration requires
an upstream Tonic update or a separately reviewed compatibility patch/fork;
the source API and browser tests do not imply that this dependency gate passed.
