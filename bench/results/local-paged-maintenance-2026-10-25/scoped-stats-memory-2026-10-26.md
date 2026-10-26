# Scoped statistics must not copy unrelated native payloads

A `SHOW STATS` request for an empty collection could allocate another collection's
entire payload. On frozen source `7d682056e9f988390db0a84897008e9f40ec8949`,
a native paged fixture with 256 unrelated 64 KiB JSON rows allocated 17,267,285
additional bytes for the empty document target, against a 455,879-byte control.
The empty table target allocated 17,267,239 bytes. The two targets stayed empty.
The regression failed its unchanged allowance of control plus 2 MiB.

## Cause and correction

Expression resolution clears `TableQuery.filter` after resolving `where_expr`.
The statistics selector only read the legacy filter, so even a scoped request
could profile every collection. It now reads `effective_table_filter` and finds
mandatory collection equalities through conjunctions. It does not descend into
OR branches; final predicate evaluation still decides which result rows survive.

The document, KV, graph, vector, queue and timeseries profilers independently
called `store.query_all` and filtered collection names afterward. They now obtain
that collection's manager before materializing entities. Owned-row schema
restoration and existing metric computation remain in the same manager path.
An isolated change to predicate recognition was insufficient while those global
materializations remained. The complete correction addresses both mechanisms.

These measurements use persistent native paged storage, matching the affected
staging profile. An earlier embedded control also copied pending embedded WAL
payloads to count records. That separate behavior is not attributed to the
staging paged OOM and is not repaired by this patch.

## Verification

The original native fixture after the correction allocated 487,528 additional
bytes for the document target and 487,482 for the table target. This is a
measured reduction of about 97% in this fixture's transient allocation, not a
process RSS or fleet-memory reduction claim.

The permanent `scoped_catalog_stats_memory` regression covers table/document
requests, conjunctions, OR result membership, the original 256-row count after
checkpoint and cold reopen, and the cold scoped allocation bound. Its final
observations were 487,396 / 487,420 / 491,561 / 495,641 additional bytes for the
four measured requests, against a 455,771-byte control. All bounds and recovery
checks passed. The existing 41 `e2e_red_schema` tests also passed, including
model-specific metrics, tenant restrictions, budget sections and consolidation
counters. Locked server lib/tests checking, workspace format and diff checking
passed. Heavy local commands ran serially in the accepted 8 GiB RAM / 4 GiB swap
scope with two CPUs.

Commands, with the existing local build-profile environment:

```sh
cargo +1.95.0 test --locked -p reddb-io-server --test scoped_catalog_stats_memory -- --nocapture --test-threads=1
cargo +1.95.0 test --locked -p reddb-io --test grouped_sql_core e2e_red_schema:: -- --nocapture --test-threads=1
cargo +1.95.0 check --locked -p reddb-io-server --lib --tests
cargo +1.95.0 fmt --all -- --check
```

## Limits and operational gate

This removes unrelated collection payloads from these profiling allocations.
The requested collection can still be materialized, unscoped statistics still
profile all collections, and large document projection/histogram buffers need
further work for a shared hard peak-memory guarantee. In-memory entity loading,
resident indexes, allocator retention and the staging process's remaining RSS
are not fully attributed or fixed by this change. No live diagnostic query was
repeated to provoke another OOM.

This is a read-side profiling change. It changes no persistent format, WAL
ordering, page publication or retirement rule. The historical RC380 result is
from the preceding frozen source; it cannot qualify a later artifact containing
this patch. Full CI, publication verification and exact-artifact Auth/Platform
qualification remain required before fleet rollout or OCI volume reduction.
