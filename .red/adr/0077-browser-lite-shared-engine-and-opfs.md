# ADR 0077 — Browser Lite, shared engine and OPFS

Status: accepted architecture; persistence foundation implemented, engine and sync pending
Date: 2026-10-22

## Decision

RedDB Lite will run a shared Rust engine in WebAssembly inside a dedicated
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
Lite requires a deliberate follow-up extraction of that executor/pager boundary,
with native and browser conformance evidence; a second JavaScript evaluator is
not an acceptable shortcut. This foundation exports RQL **validation**, not query
execution. The private `@reddb-io/lite` package cannot be released as the database
SDK before the shared executor is working.

New sync contracts belong to `reddb-wire` (ADR 0046). They require consistent
snapshot/cursor bootstrap, durable resumability, transactional/idempotent upload,
stable cross-device identities and graph references, deletes/retractions, and
normal server-side authorization/RLS. The in-memory CDC notification ring is not
a durable offline sync protocol. Rebootstrap after cursor expiry must preserve
pending intent. Local transactions are atomic locally; offline device writes do
not imply global serializability. Permission revocation reaches an offline
device upon reconnection, not retroactively.

ADR 0047's existing RedUI bridge and remote transport remain unchanged. Any Lite
integration into that UI is a separate SDK/UI change, not a claim that a hosted
page can access a native `file://` path.

## Verification

The foundation requires canonical native/portable file interoperability, failures
at every publication write/flush, published-WAL corruption detection, native
artifact regression tests, and real browser OPFS restart/ownership/corruption
tests on Chromium, Firefox and WebKit. Full Lite additionally requires five-model
query/transaction parity, real Safari/iOS storage checks and two-device sync
under conflicts, duplicates, cursor expiry and permission changes. Bundle size,
startup, memory, storage and latency measurements precede performance claims.
