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
Budget failures do not advance the cursor; I/O/corruption must fail the operation.

A cold collection retains its immutable base and a resident mutation overlay.
Cold updates materialize their target row; physical deletion masks its base row.
Checkpoint merges into an unpublished replacement and uses the existing ownership,
WAL, metadata publication and page-retirement fences. Captured readers retain the
previous generation. Pending transaction/Vault rows prevent overlay eviction.
The runtime's fallible scan adapter propagates storage errors alongside CALL/query
work-budget errors, including when no CALL budget is installed.

Cost sketch: cold open currently decodes N records once with O(1) record payload
memory; scans retain O(dirty IDs + one decoded row), and a changed collection's
checkpoint currently copies O(N + dirty rows) bytes. These are intermediate costs,
not the required constant-time boot or paced maintenance implementation.

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

- Durable generation-bound counts/identity high-water metadata, safe legacy
  preparation, WAL recovery and older-writer handling for prepared fast boot.
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
