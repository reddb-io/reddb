# Row and index memory admission

The row mutation engine now reserves estimated segment and secondary-index
growth together, before either write kernel runs. This covers the existing row
entry points that converge on `MutationEngine::apply`, including single-row and
batch inserts, and builds on [concurrent reservations](concurrent-memory-reservations.md).

The estimate visits registered indexes without cloning the registry or row
payloads. Hash and bitmap entries include key and posting overhead. Single-column
BTree reserves its sorted backing and auxiliary equality hash; composite BTree
includes every tuple component. H3 uses the size of its canonical cell key.
The prospective automatic `id` hash includes its initial backing structure and
row entries. It follows the existing first-row trigger, single-column coverage
rule and `auto_index_id` opt-out. Missing fields do not add index entries.

Admission combines these bytes with the segment estimate in the existing single
reservation. A budget rejection occurs before row publication and implicit index
creation. The reservation remains alive across index maintenance and ordinary
error cleanup, following the existing completed-reservation sampling protocol.

Single-row composite maintenance now updates the composite selected by each
registry entry. Previously each composite entry invoked collection-wide composite
maintenance, so two distinct composite indexes received two postings per inserted
row. Tuple construction is shared with batch and collection-wide maintenance.
This prevents new duplicate postings; it does not repair indexes already populated
by the old behavior. Rebuilding affected indexes is a separate operational action.

## Cost and bounds

Cost sketch: N rows × K affected indexes × field lookup, with one borrowed registry
read and the existing one budget sample per mutation batch. Ordinary stored fields
add no heap allocations in the estimator. Text/scalar key lengths are direct;
other hash encodings count formatted bytes without allocating a second encoded
buffer. Those fallback encodings still cost O(encoded bytes). Document-path
resolution reuses the indexer's resolver and can allocate decoded values.
Single-row composite lookup borrows stored keys without allocating lookup copies;
it scans the existing composite inventory instead of updating every composite.

The allowance is conservative: it assumes potential new keys and can charge
unsupported sorted values or invalid geo values that the backend will skip.
Resident estimates still exclude allocator capacity slack and temporary write or
query buffers. Existing rows sharing a posting list may use less than reserved.
This is not a hard process RSS guarantee or latency/throughput benchmark.

The index registry is sampled before admission, not locked across mutation. DDL
that changes index topology concurrently still needs a separate synchronization
contract. Other model/update paths, complete metadata estimates, explicit index
build/timeseries estimator alignment, query buffers and maintenance headroom remain
follow-up work. WAL formats and transaction rollback semantics are unchanged.

## Verification

Regressions reject single and batch inserts when only their row estimates fit,
verify no rows or implicit index were created, and retry after releasing a
competing reservation. Disabling auto-indexing admits the row-only estimate.
Index tests compare estimated growth with resident counter deltas across hash,
bitmap, sorted, composite and H3 backings; verify automatic-index header accounting;
and prevent single-row fan-out across distinct composite indexes. An allocation
counter verifies that estimating stored fields, including debug-encoded blob keys,
allocates no heap buffers.
