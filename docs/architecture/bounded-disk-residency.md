# Bounded disk residency implementation

This implements the server disk-residency work in ADR 0038 phase 4 under the
shared memory governance of ADR 0073. It is development work, disabled in the
runtime factory. `UnifiedStoreConfig::resident_memory_bytes_max` is an internal
opt-in for storage acceptance tests, not a qualified server memory ceiling.

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
Checkpoint merges into an unpublished replacement and uses the existing ownership,
WAL, metadata publication and page-retirement fences. Captured readers retain the
previous generation. Pending transaction/Vault rows prevent overlay eviction.
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
propagate through SELECT, DML, UNIQUE and the binary wire response. Legacy IN
unions and sorted/bitmap intersections that cannot retain an owned allocation
skip the paged optimization; these paths still need an admitted streaming design.
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
