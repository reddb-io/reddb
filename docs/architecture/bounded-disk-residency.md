# Bounded disk residency implementation

This implements the server disk-residency work in ADR 0038 phase 4 under the
shared memory governance of ADR 0073. It is development work, disabled by default.
The real server can opt in with `red server --bounded-disk-residency`; library
hosts use `RedDBOptions::with_bounded_disk_residency(true)`. Both select the same
factory and derive the resident segment cap from the `SegmentArena` share of the
resolved memory budget (`REDDB_MEMORY_BUDGET` for the server). This is not yet a
qualified server memory ceiling: the remaining admission gaps below still apply.

The explicit option supports standalone, embedded/single-file, local storage.
Other profiles, replication roles and remote backends fail before opening the
store. Managed/default activation remains disabled pending qualification.
New databases use the native pager from first boot. Existing native paged files
can be prepared; legacy snapshot containers are rejected without conversion and
must first be restored into an independent paged copy.
Preparing a database upgrades its writer format; older and non-opted-in writers
reject prepared files. Retain an independent, unconverted copy for rollback.
Preparation scans legacy data and must be measured separately from prepared boot.

The motivating Platform measurement retained 2,366,465,376 bytes in segments for
805,776 physical records, in addition to 644,055,040 page-cache bytes and
114,259,834 index bytes. Operational history is real data and remains present.

## Read and publication contract

A published B-tree generation supplies fallible point reads and ordered cursors.
Encoded overflow/compression allocations are rejected before expansion when they
exceed the caller's byte allowance. Physical keys must match decoded entity IDs.
Encoded cursor budget failures do not advance the cursor; I/O/corruption must fail
the operation. Typed decoding additionally admits encoded input, expanded native
text/blob values, vectors, arrays, maps, metadata and auxiliary entity containers
before their allocation. All fields share one local decode allowance; bounded
native-value denial preserves its source position for retry. Native record/value
bytes remain unchanged. Hosts must still reserve these local allowances against
the global scratch budget, including transient codec contexts and transport data.

Decoder cost sketch: one O(encoded entity) parse with O(1) admission per allocation,
plus an O(encoded metadata) preflight using a fixed depth-bounded explicit stack;
metadata is usually absent. This is not measured hot-query performance evidence.

A cold collection retains its immutable base and a resident mutation overlay.
Cold updates materialize their target row; physical deletion masks its base row.
Checkpoint normally merges into an unpublished replacement using the existing
ownership, WAL, metadata publication and page-retirement fences. Committed
row inserts without logical keys can instead use the existing sparse WAL-image
projection when the backing has no external strong/weak owners or cursor pins.
Compatible unpinned secondary projections, including runtime COUNT visibility
indexes, append their new postings in the same ownership epoch. Exact projector
identities must match, and retained posting work is capped at 1 MiB before allocation. New readers are fenced while verifying and modifying that base. Page
undo protects splits and overflow allocations; exact counts and the XID upper
bound, including appended committed creators, follow a changed root. Captured readers, changed/missing index definitions, updates, deletions, keyed
rows, MVCC changes and exhausted mutation/WAL proofs retain the replacement path.
That fallback still costs O(history + mutations); this optimization does not make
all collection checkpoints incremental. Captured readers retain their generation. Pending transaction/Vault rows prevent overlay eviction.
The runtime's fallible scan adapter propagates storage errors alongside CALL/query
work-budget errors, including when no CALL budget is installed.

The keyed runtime resolver propagates disk/decode failures, including REST and
mutation pre-reads. Resident collections retain their segment key indexes.
Prepared cold bases seek a paired, immutable SHA-256 candidate index and verify
the decoded logical key before accepting it. Physical replacements/deletions mask
old candidates, and versioned reads select the snapshot-visible maximum xmin.
Both data and key roots publish in the same checksummed resume record and share
ownership/retirement fences. Index records cost a 40-byte key plus B-tree slot
space; seeks cost O(tree height + physical versions/collisions of this key).
These are cost sketches, not measured hot-query performance evidence.

A legacy positional row without durable column names prevents a trusted key
index. Its prepared state explicitly retains the fallible streaming fallback,
rather than claiming that unclassified keys are absent or rebuilding every boot.
Legacy preparation streams payloads for counters and a second index-building
pass; prepared reopen reads metadata, not the payloads. Current Config/Vault
history/list readers still stream their relevant collections; indexed candidate
access remains to be connected to those iterators before performance qualification.

Config history/latest/list reads stream the physical base and propagate faults;
Vault snapshot reads retain frozen metadata, tenant checks and encrypted history.
Raw physical cursors preserve superseded versions for encrypted Vault export.
Store config reads and idempotent boot reseeding have fallible APIs; bootstrap
completion and manifest-registry decisions use them so unreadable durable state
cannot be mistaken for first boot. Legacy best-effort wrappers remain for callers
not yet migrated and still block managed activation. Retained output objects and
frozen overlay copies still require global admission.
Cold counter pre-reads preserve TTL and tags, and storage faults propagate before
mutation. Listing/setting VCS opt-in flags also scans fallibly; re-enabling writes
the replacement before deleting prior flags so a failed insert preserves history.

Prepared files carry checksummed resume counters alongside their exact collection
roots: physical counts and entity/row/sequence allocator high-water marks. A preparation scan
is still required for legacy files. Prepared boot follows metadata and one tree
path, without decoding collection payloads; a short committed WAL remains as a
resident overlay until checkpoint publication. Repeated opens retain that WAL.

The preparation contract fences file version 1.0.4 in both recoverable superblocks
before publishing trusted counters. Older engines reject it. Current pager hosts
also reject a prepared file for writing unless they opt into disk residency;
read-only inspection remains possible. Default new files retain version 1.0.3.
The capability flag is a host storage contract, not runtime activation. Preserve a
preparation copy for downgrade; no downgrade writer may reinterpret these counters.

Native read-only opens acquire a shared file lock and reject an active writer.
They open the pager without write permission, restore double-write and checkpoint
before-images into a private page overlay, and replay committed WAL changes into
the resident view. They do not checkpoint, consume WAL, reclaim pages, migrate the
manifest, clean up pending drops, or start a primary replication spool. Pending
collection drops are reflected in the reader's view without deleting artifacts.
Repeated opens leave source file contents and modification times unchanged.

With bounded disk residency enabled, compatible published indexes and collection
backings remain paged. Without it, read-only opens hydrate collections, including
prepared files. Missing secondary projections use the existing scan fallback;
read-only boot never publishes a replacement index. Recovery images are pinned
separately from the evictable page cache and included in resident page accounting.
Their count is capped at the configured page-cache slots, with a minimum of one
complete DWB frame. Recovery exceeding that cap fails without modifying the
source; increase the page-cache allowance or recover an independent writable copy.

Cost sketch: legacy preparation makes bounded-memory record passes for counters
and key candidates; prepared boot reads O(metadata + tree height + short WAL). Scans retain
O(dirty IDs + one decoded row), and a changed collection's checkpoint currently
copies O(N + dirty rows) bytes. These intermediate algorithms do not establish the
required measured boot latency or paced maintenance implementation.

## Secondary-index publication foundation (2026-11-01)

`UnifiedStore::register_secondary_index` registers a deterministic bounded-key
projector for checkpoint preparation. Its identity includes an explicit codec
version and a fingerprint of the complete definition. Checkpoint builds all
changed projections for a collection in one bounded source pass, appends each
physical entity ID to its projected key, and publishes secondary roots alongside
the exact data root. Reusing a generation or definition never requires rebuilding
that index. A changed data root without an available projector invalidates the
index; callers must rebuild or scan, never trust the preceding generation.

Catalog resume version 5 carries these descriptors. The writer fences file
version 1.0.5 in both superblocks before building them, so older engines reject
the file before recovery can release pages belonging to an unknown tree class.
Files without secondary roots retain the v4 catalog representation; a file fence
is never downgraded after DROP. Secondary roots may not alias source, key, other
secondary or retired roots. Empty prepared generations retain their empty index
identities on reopen.

`SecondaryIndexRead` provides an immutable, fallible candidate cursor. It retains
one leaf and a bounded key rather than materializing the entire posting list.
The cursor independently pins retirement after its read handle is dropped.
Checkpoint retirement, interrupted ownership recovery, DROP and definition
replacement use the existing page-ownership protocol. A projection failure
fences further writes until recovery and cannot publish a partial index.

Resource sketch: O(N * changed indexes) compact postings are written during
preparation, holding one decoded source row, two 1-KiB key buffers and O(indexes)
tree handles. Reopen retains O(indexes) descriptors; a seek reads O(tree height +
matching postings). The primary-key preparation still has its separate pass.
This is not a measured boot/maintenance claim or global memory admission.

## Runtime HASH integration (2026-11-01)

The opt-in cold runtime installs HASH providers with immutable definition/schema
fingerprints instead of resident posting maps. Restore registers all projectors
before a shared checkpoint builds missing roots; a compatible prepared root is
reused. Standalone BTREE/H3/bitmap providers and BTREE equality pockets remain
resident and still perform their existing restoration passes.

HASH seeks use a SHA-256 projection of the historical canonical key encoding,
then verify the complete raw key against current physical source rows. Composite
and debug-encoded keys have a preflight expansion bound before allocation. The
publication fence covers native-root capture, source recheck and the resident
WAL overlay. Physical deletions/replacements mask old candidates, and in-place
sorting removes duplicate physical IDs without a second HashSet. Snapshot-aware
UNIQUE admission still distinguishes active, aborted and superseded versions;
its source re-read is now fallible and owns decode scratch.

Lookups start with a 64-KiB decode/candidate allowance and retry larger rows or
posting lists with admitted geometric growth up to the configured ceiling.
Candidate vectors are bounded and admitted before allocation. Their reservation
travels with the result and its consuming iterator. Exhaustion produces an error,
never an incomplete successful list. Missing/incompatible roots use a fallible
source scan until checkpoint publication. I/O failures in the selected HASH path
propagate through SELECT, DML, UNIQUE and the binary wire response. IN unions
and sorted/bitmap intersections retain resident owned allocations, but still
skip paged HASH providers; their disk integration needs an admitted streaming design.
Catalog entry counts are NULL for paged HASH until exact cardinality is persisted;
an unknown count is not reported as zero.

Resource sketch: retained HASH metadata is O(index definitions), lookup scratch
and candidates share the runtime budget, and lookup work is O(tree height +
matching physical postings + resident overlay). Publication currently serializes
this lookup with checkpoint/mutation; its latency impact still needs measurement.
Explicit CREATE INDEX still stages its existing resident construction before
installing the paged provider, so that transient peak is not solved by this slice.
This integration does not enable the managed factory or establish the global
server/transport memory ceiling, boot target, or STG qualification.

## Compact exact COUNT projection (2026-11-01)

The opt-in runtime prepares an internal secondary projection containing xmin,
xmax and moderation visibility for every physical record. Physical identity is
the posting suffix. The codec/schema fingerprint and exact data generation must
match before a query trusts the root. Initial preparation shares the secondary
projection pass and may read all payloads once; it is not a prepared boot timing.
Subsequent opens reuse compatible roots.

Unfiltered scalar `COUNT(*)` scans these compact keys, hides base postings
replaced/deleted by the resident overlay, and counts resident versions under the
same captured snapshot. It uses the existing transaction-outcome predicate,
including active, aborted and own savepoint writers. Moderation markers in both
named and positional rows remain hidden. It never treats physical catalog
cardinality as a visible count. Serializable readers retain the entity scan for
logical read tracking; predicates (including tenant/RLS), column counts, grouping,
HAVING, ordering, expansion and CALL work budgets also retain the existing path.
Missing/incompatible projections fall back; malformed/unreadable projections fail
the query. New collections can use the scan until their next runtime preparation.

Cost sketch: 17 visibility bytes plus an 8-byte identity and native slot overhead
per physical version; counting reads O(compact index pages + overlay rows), keeps
one immutable leaf, borrows keys without per-row allocation, and never decodes
base payloads. It uses the existing publication fence through base/overlay merge;
concurrent-write latency still needs qualification. This slice does not establish
global admission, HTTP behavior, boot p95 or a release performance result.

## Paged BTREE restoration and composite ranges (2026-11-01)

The opt-in runtime restores scalar and fixed-arity composite BTREE indexes as
generation-bound native roots. Initial preparation scans source rows to build
prefix-free ordered tuples; compatible reopen retains index descriptors without
rebuilding resident posting maps. UNIQUE and scalar equality use paged equality
projections and preserve snapshot-aware conflict checks.

Integral and text ranges support equality on every preceding composite column.
Planning prefers the applicable index with the longest constrained prefix. Each
component's canonical family is checked within its prefix; mixed families,
unsupported predicates and oversized tuples fall back to the authoritative scan.
Native page errors propagate. Candidate IDs retain their shared-budget reservation
while source-key rechecks and SQL visibility checks reconcile resident mutations.

Regression tests deny source-wide scans during prepared restoration and queries,
including two-, three- and five-column indexes, prefix changes, rollback and
UNIQUE after reopen. These tests establish behavior, not historical boot latency.
Bitmap/H3 restoration and explicit CREATE INDEX still use resident construction;
full-server admission and the acceptance gates below remain required.

## Required acceptance gates

- Platform total process/container memory <=1 GiB, with a 768 MiB engine budget,
  including boot, concurrent query/results and maintenance peaks.
- Hot-query p95 latency and throughput regressions <=10% on identical hardware.
- Prepared boot p95 <=5 seconds across at least 30 opens, measured from process
  start to the first authenticated query with volume available and conversion
  already complete. Initial conversion uses one maintenance window per STG DB.
- Real server tests with 1/4/16 concurrent clients and the 2,000-query workload;
  proposed p95 point lookup <=50 ms and exact COUNT <=2 seconds.
- Records, history, models, indexes, authentication, transaction outcomes and crash
  recovery remain intact. No retention reduction or weaker durability to pass.
- Identical immutable artifact tested on isolated OCI Auth/Platform copies, with
  datasets 1x/4x/16x the budget, >=100 write/delete/checkpoint/drain cycles and a
  24-hour workload, before minor publication and all four STG upgrades.
- At least 100 constant-live-data churn cycles after warmup, with allocated
  storage growth <=64 MiB, and no skipped/corrupt data or index keys.

## RedWire response ownership

Post-handshake replies previously used a queue bounded by frame count alone.
An encoded reply could remain queued or blocked in a socket write after query
result accounting had returned to zero. The transmitter now reserves the exact
encoded capacity plus the existing payload's capacity against the runtime budget
before copying. It drops the payload before shrinking the reservation, and sends
shared byte owners that retain the encoded allocation's charge through clones,
queueing, blocked sends and socket writes. Cancelled sends release their own
buffer without releasing other queued responses.

Memory refusal fails the connection. The writer observes the same failure while
blocked on a slow socket, drops queued and in-flight byte owners, and wakes
blocked producers. Later StreamEnd frames cannot turn a discarded chunk into
apparent success. Socket errors also wake the session. Clean Bye/half-close keeps
the established behavior of draining accepted responses before EOF.

The canonical uncompressed codec accepts caller-owned capacity. Existing server
builders emit uncompressed replies; an outbound compression request uses the
wire protocol's plaintext fallback, clearing only COMPRESSED, until compressor
workspace admission exists. Shared codec compression and incoming compressed
frames are unchanged. This can increase traffic for future producers that request
compression. Frames exceeding the wire size limit fail before the encoding copy.

Regression tests cover shared ownership, pre-copy refusal, encoder unwinding,
full-queue cancellation, a blocked duplex socket, failure cleanup and recovery,
and 1/4/16 transmitters sharing one budget. These are transport ownership tests,
not the authenticated-server workload/latency qualification above. Per-row
presentation trees, command payloads, gRPC/protobuf, handshake and executor
workspaces still need their own pre-allocation admission. It does not establish a total
RSS bound, storage plateau, or permission to activate managed disk residency.

RedWire output streams now execute through the request seam and keep the
materialized-read owner while streaming, including when a slow peer blocks a
chunk. Cancellation or failure releases the result and pending encoded owner.
Rows render into JSON individually rather than retaining a second complete
JSON row vector. Query execution itself still materializes all records, and
single-row presentation/serialization allocations precede frame admission;
this is not a pull-based scan or complete stream peak-memory bound.

An unencodable row now emits `stream_chunk_encoding_failed` before the terminal
frame. Previously a frame-builder refusal broke the loop and reported StreamEnd
without an error, silently dropping the row. Regressions exercise an actual
blocked stream worker and a row larger than the wire frame limit.

The legacy binary result encoder no longer keeps a thread-local scratch vector.
A 192 KiB response followed by a small response previously left 196,628 requested
allocation bytes retained by one worker after both responses were dropped. The
encoder now appends into its final legacy frame through a shared wire helper,
removing both complete intermediate payload copies. Regression measurements on
1/4/16 fresh workers require allocated and released bytes to balance after both
responses drop, and bound cumulative allocation to prevent full-buffer copies
from returning. The test allocator counts successful resize replacement bytes;
these are requested allocation measurements, not allocator-resident RSS or
latency benchmarks. Wire bytes, column resolution and value encoding are unchanged.
Materialized binary reads now admit column metadata and exact serialized capacity
before allocation. Borrowed value encoding avoids temporary text/blob copies.
The admitted legacy owner stays charged while the RedWire transmitter reserves
and copies into its final output owner, without an intermediate payload copy.
Committed command acknowledgements retain their existing no-late-read-refusal rule.

Direct binary scans also admit their output before allocation. They sample storage
once outside the scan, then reserve geometric buffer growth through a short ledger
shared with mutation admission. The ledger never samples storage or waits on the
sampler mutex, avoiding a storage-lock/sampler-lock cycle inside callbacks. Both
old and new capacities remain charged during growth; a refused row discards the
accepted prefix and returns an error without retrying through the fallback path.
LIMIT, hash equality, BETWEEN and composite scans keep their direct execution paths.
Ordered scans admit candidate/slot workspaces and individual row buffers, then
assemble rows in candidate order even when segment traversal visits another order.
Empty results retain admitted headers. The canonical wire codec patches the legacy
envelope in place, without another allocation.

Cost sketch: geometric growth copies less than twice the final capacity across
resizes, uses one ledger lock per growth, and performs no storage resampling per
row; ordered row buffers allocate their exact encoded length. This is an algorithmic
bound, not a measured latency or whole-process RSS qualification. Parser,
backing-read and predicate workspaces still need admission;
charging the response does not account for those earlier allocations.

Resident HASH point lookups and owned `IN` unions reserve their entire candidate
allocation before copying IDs. Sampling happens outside the manager lock, then
the backing is rechecked for concurrent DROP/replacement. The frozen posting lists
use the storage-independent ledger for admission. Candidate owners retain the
reservation through consuming iterators; failure discards the whole operation.
Empty lookups do not allocate or sample, preserving the cheap UNIQUE-miss path.
Paged point providers keep their existing admission; paged unions retain the
bounded source-scan fallback. Resident sorted/bitmap intersections now retain
owned candidates as described below; no raw-vector conversion strips a reservation.

Cost sketch: N candidate IDs require N × 8 bytes plus one reservation owner;
a resident union counts and copies K posting lists without per-key temporary
allocations or an intermediate union copy. Nonempty lookups add one sample and
one admission-ledger reservation; no latency claim is made without benchmarks.

## Resident index intersections (2026-11-02)

Sorted range, IN and composite lookups count and reserve their exact candidate
allocation before copying. They sample outside index locks and reselect/freeze the
current posting generation before counting, admission and copying. Memory refusal
is an error through table, DML and direct binary callers, never a silent scan retry.
The existing range break-even fallback and leaf LIMIT semantics remain in place.
Intersections retain both candidate owners while admitting the smaller-side
HashSet and final output, including the consuming iterator's lifetime.

Resident multi-equality bitmap intersections retain full u64 EntityIds through
RoaringTreemap; the former u32 narrowing could alias unrelated rows. Bitmap
metadata, container growth/conversion and AND scratch are reserved before building
the bitmap. Candidates sort in place; intermediate bitmap owners are released
before the final set/output when their data is no longer needed. Paged HASH union
and paged BTREE fallback behavior is unchanged.

Cost sketch: an N-ID vector uses 8N bytes plus its reservation owner; the
intersection set reserves 32N + 64 bytes (bucket rounding/control-byte headroom).
Each bitmap high-32 partition reserves 1024 bytes; each high-16 container reserves
128 bytes plus three u16 payload capacities (next power of two, clamped to
4..4096 IDs), covering array growth, bitmap conversion and AND scratch for roaring
0.11.4. Counting adds an index traversal and bitmap preparation sorts in place;
the Roaring intersection fast path is retained. These are allocation bounds,
not measured latency or RSS claims.

Regressions cover the public LIMIT-1 direct query under pressure, denial at leaf,
set and final-output admission, DML refusal without row changes, shared admission
with 1/4/16 owners, dense/sparse/full-width bitmaps and allocation peaks, plus
sampling outside locks and selection of the current index generation. Canonical
predicate keys, composite/name metadata, legacy H3/graph raw candidates and covered
values remain separate workspaces; this slice does not establish full-server or
STG qualification.

## Cold Control Event Ledger reads

`red.control_events` now scans the authoritative disk base and resident overlay.
The previous resident-only snapshot returned zero persisted audit events after a
bounded-residency reopen. Tenant scope and fallible WHERE evaluation precede
retention; storage corruption and decode-limit errors propagate to the caller.
Unordered LIMIT/OFFSET reads skip without retaining the prefix and stop early.
Ordered reads periodically retain only the best OFFSET + LIMIT rows, bounded by
the materialized-row ceiling and shared query-buffer admission. The candidate
batch also counts toward that ceiling; a window filling the ceiling can fail
rather than bypass the configured cap.

The common `ORDER BY ts DESC, id DESC LIMIT ...` page uses an internal native
secondary tree over descending signed timestamp and fixed-width ledger ID.
Preparation joins the existing checkpoint epoch; compatible reopen restores the
root without reading the ledger. All internal projectors are registered before
runtime-index restoration can checkpoint missing projections. Otherwise recovery
of a newly created collection can force a full audit-history rewrite and then a
second index-build checkpoint. A process-exit regression verifies that a new KV
collection plus an acknowledged audit append retains the prepared audit root.
Sparse append maintains the posting alongside
the source row. Reads fence publication, merge visible resident rows with the
requested qualifying base window, skip shadowed/deleted base identities, and
verify every posting against its source. Scope and residual predicates still
apply before counting a row toward the window. Selective predicates can require
more candidates; this is not an arbitrary-filter latency guarantee.

Unsupported legacy IDs/timestamps retain an explicit fallback marker; missing
or incompatible trees use the bounded authoritative scan, while unreadable
postings fail. Other orderings use the existing bounded sort. Initial index
preparation is migration work, measured separately from prepared boot.

A per-row decode allowance of min(engine budget / 16, 16 MiB), with a second
reservation for schema projection, prevents arbitrarily large ledger rows from
being decoded without admission. Retained ordinary values, vector growth and
sort workspaces are charged before growth and released on pruning or error.
The workspace owner survives ordinary projection construction. This is not a
claim that arbitrary expression expansion or every transport is admitted; those
remain in the activation gates below.

`cold_control_events` covers cold-plus-overlay visibility, aliases/filtering and
pagination, tenant isolation, a 1,024-event sort under a 32-row ceiling, rejection
when the byte budget is exhausted, error cleanup, oversized decoding, and a
wrong-type backing page that must never become an empty successful ledger. It
also denies source-wide reads to prove an indexed page fetches only its candidate
window, and compares filtering/tenancy, overlay deletion, sparse checkpoint/reopen
and legacy fallback against the full scan. It runs in `make test-storage-apocalypse`
as well as the ordinary integration suite.

## Cold logical catalog reads

The fallible catalog path includes the native base and resident replacements,
insertions and deletions. Legacy collections without declared contracts retain
their inferred model, schema mode, entity count and cross-reference count after
reopen. Declared contracts remain authoritative; catalog observation does not
rewrite them. The former resident-only observation could report a persisted
legacy table as mixed/dynamic with zero entities.

`RedDBRuntime::try_catalog` admits one bounded decode workspace through the shared
query-buffer ledger. SQL catalog, typed-model and dependent operational views use
this fallible path. Queue-policy metadata is also streamed from its native base.
Corrupt pages and decode/admission errors reject the query rather than returning
an apparently successful empty catalog. Existing infallible library adapters
retain their resident compatibility behavior; disk-resident callers must use the
fallible API.

The order-independent manager visitor retains one native leaf and decoded row,
without an ID vector or payload clone of the resident overlay. Its host holds the
publication fence throughout the reduction to prevent concurrent materialization
from counting a base row and its replacement twice. Cost is O(physical records)
I/O/CPU and O(one decoded record + catalog descriptors) memory. This is correctness
and bounded-workspace evidence, not a catalog-latency or concurrent-write claim;
historical performance qualification remains required before activation.

`cold_catalog` covers the SQL path before/after repeated reopen, typed legacy
collections, base/overlay masking, publication, shared-budget denial and corrupt
backing pages. Existing storage, audit-recovery, queue-policy and query-behavior
suites run alongside it.

## Committed-write draining and native history reclamation (2026-11-03)

Bounded paged runtimes check resident-arena pressure before mutation admission.
At three quarters of the arena allowance they attempt the existing local
checkpoint, retaining its WAL, auxiliary snapshots, metadata and publication
fences, without uploading a remote snapshot. A busy publication fence is not
waited on from mutation admission. Nested publication, deferred WAL captures,
active calling transactions, startup and read-only runtimes skip this drain;
ordinary shared-budget admission remains authoritative. SQL DML also checks
before starting event capture. Local checkpoints prepare visibility projections
for collections created since startup before publishing their first cold base.
This is a synchronous checkpoint trigger, not a
bounded-latency incremental checkpoint implementation; replacement fallbacks
and pinned readers still require qualification.

A checkpoint marks every omitted or normalized transaction/Vault row as still
unprojected. It cannot replace that collection's resident overlay with its
durable base until those images match. Sparse checkpoints recompute this state
after revisiting deferred IDs, including commit and rollback outcomes.

MVCC maintenance visits both weak resident positions and a native base pinned
only within one fenced tick. Between ticks it retains a bounded resume posting,
including its physical ID, rather than data/index generations. A paused sweep
therefore cannot force otherwise eligible appends through full-history copying.
When its compatible visibility projection exists, cleanup reads compact XID
postings and skips live payloads. Unprepared bases retain the primary-ID cursor
fallback. Each reclaimable candidate is read again from the current manager
under the publication fence before reclamation. Superseded
cursor images cannot delete a revived row, an active creator or a version still
needed by a retained reader. Work is limited by inspected positions, reclaimed
versions and decoded bytes, with one current row and leaf retained. Runtime
maintenance reserves its encoded/decoded workspace through shared admission.
I/O and decode failures propagate; cleanup does not interpret them as missing
records. Physical removals use the existing WAL-backed deletion path.

Regression fixtures use fixed payloads and explicit process-exit boundaries.
The constant-live-data test stops background tasks and drives the production GC
in bounded steps, checking exact removal counts, checkpoint/reopen payloads and
peak allocated growth over 100 cycles after warmup. It does not rely on sleeps,
wall-clock deadlines or background scheduling. Latency and concurrency/load
measurements remain separate activation evidence.

## Remaining implementation before activation

- Connect the remaining persisted secondary trees to runtime providers and
  qualify compact MVCC-aware COUNT; extend scan projections where needed.
- Qualify prepared metadata publication against I/O failures and superblock
  fallback, then measure boot on the exact OCI artifact and full historical data.
- All authoritative reads/scans/model/index/auth paths migrated to fallible access;
  legacy resident-only and best-effort adapters are not safe for activation.
- Global accounting/admission for decoded objects, overlay identities/deletions,
  scratch space, index state, query results and serialization until transport ends.
- Paced overlay draining/checkpoint updates; bounded spill/merge/temp-disk quota
  with deterministic cleanup and progress reserved for WAL/commit/health.
- Transaction/Vault rollback, all models, I/O/corruption/publication fault campaigns,
  complete runtime/client flows, performance and OCI/soak qualification above.

Low-level acceptance is covered by `bounded_paged_read` (engine) and
`bounded_disk_residency` (server). Those tests alone do not qualify a release or
prove the process memory and latency gates.
