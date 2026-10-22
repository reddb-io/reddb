# ADR 0077 — RedDB Browser, shared engine and OPFS

Status: accepted architecture; persistence, portable query operators and page primitives implemented; pager I/O, runtime and sync pending
Date: 2026-10-22

## Decision

RedDB Browser will run a shared Rust engine in WebAssembly inside a dedicated
worker. Its first usable database release must support tables, documents, KV,
graph and vectors, offline writes, and synchronization of a server-authorized
subset. Conflicting writes are preserved for explicit application resolution.

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
MVCC, pager and storage-backed query orchestration remain in `reddb-server`.
Canonical output-column labels and binary document decoding also have one owner.

The page primitives now also live in `reddb-engine::storage`: 16 KiB pages,
CRC32, the freelist and SIEVE page cache. Native module paths alias those exact
types and implementations. Their persisted encodings still belong to
`reddb-file`; the move changes neither bytes nor cache/allocation algorithms.
Native unit tests moved with the code, including the native cache concurrency
tests. Shared native/WASM fixtures cover page headers and cells, checksums and
corruption, multi-trunk freelist recovery, pinning, eviction and dirty writeback.
A native pager restart test checks that shared page types cross the existing
file-I/O boundary. This does not yet port the pager's WAL/DWB ordering,
allocation fences, encryption, checkpointing or MVCC into the browser.

The experimental `executeScalar` entrypoint executes storage-free SELECT
expressions in the worker with the same evaluator and exact JSON integer/decimal
tags. It rejects unsupported clauses explicitly and never reads or writes the
artifact. Operator fixtures run identically as native Rust tests and a separate
test-only WASM artifact inside browser workers. Native runtime differential tests
compare the scalar corpus against actual `RedDBRuntime` results. Query timing
uses the host's monotonic clock (`Instant` natively, `performance.now` in WASM).
The private package still cannot be released as the database SDK: storage-backed
five-model execution, local transactions and sync remain pending.

Cost sketch: filtering 10,000 rows visits 10,000 rows; comparison sorting is on
the order of 10,000 × log2(10,000), about 133,000 comparisons. The relocated row
executor retains O(N) materialization and adds no per-row IPC or storage I/O.
This is a portability step, not a throughput or browser memory-budget claim.
Page payload memory remains capacity × 16 KiB (256 resident pages = 4 MiB),
plus cache metadata and existing copies during read/writeback; no new IPC,
disk operation or allocation policy is introduced by sharing these modules.

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
