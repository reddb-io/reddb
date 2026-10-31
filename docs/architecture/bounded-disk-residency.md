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

The keyed runtime resolver also propagates disk/decode failures, including REST
and mutation pre-reads. Resident collections retain their segment key indexes;
a cold base currently uses an O(N) streaming key probe with one decoded row.
Physical replacements and deletions mask old base candidates, and versioned
reads select the snapshot-visible maximum xmin. VCS versioning settings are read
fallibly so failure cannot silently disable history during a write. A durable,
budgeted key index is required before managed activation or performance claims.

Config history/latest/list reads stream the physical base and propagate faults;
Vault snapshot reads retain frozen metadata, tenant checks and encrypted history.
Raw physical cursors preserve superseded versions for encrypted Vault export.
Store config reads and idempotent boot reseeding have fallible APIs; bootstrap
completion and manifest-registry decisions use them so unreadable durable state
cannot be mistaken for first boot. Legacy best-effort wrappers remain for callers
not yet migrated and still block managed activation. Retained output objects and
frozen overlay copies still require global admission.

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

Cost sketch: legacy preparation decodes N records once with O(1) record payload
memory; prepared boot reads O(metadata + tree height + short WAL). Scans retain
O(dirty IDs + one decoded row), and a changed collection's checkpoint currently
copies O(N + dirty rows) bytes. These intermediate algorithms do not establish the
required measured boot latency or paced maintenance implementation.

## Required acceptance gates

- Platform total process/container memory <=1 GiB, with a 768 MiB engine budget,
  including boot, concurrent query/results and maintenance peaks.
- Hot-query p95 latency and throughput regressions <=10% on identical hardware.
- Prepared boot <=2 seconds with node/image ready and short WAL.
- Records, history, models, indexes, authentication, transaction outcomes and crash
  recovery remain intact. No retention reduction or weaker durability to pass.
- Identical immutable artifact tested on isolated OCI Auth/Platform copies, with
  datasets 1x/4x/16x the budget, >=100 write/delete/checkpoint/drain cycles and a
  24-hour workload, before minor publication and all four STG upgrades.

## Remaining implementation before activation

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
