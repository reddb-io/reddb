# ClickHouse storage research and RedDB opportunities

Date: 2026-11-05. RedDB revision studied: `5515513c4d7446593a47ebbbd05b21701cf1e68d`.

## Scope and the 500-billion-token claim

The source of the reported 500 billion tokens in 5 GB was not available for
inspection. Five decimal GB divided by 500 billion is 0.01 bytes, or 0.08 bits,
per token. That is not a reasonable general expectation for losslessly storing
arbitrary token sequences. A count of tokens *processed* or *billed* can be
stored in a few integers; repeated context may be counted many times. Those
are different quantities from storing every token or the original text. Verify
the original workload, representation, repetition, retention, and what disk
usage includes before drawing a compression conclusion.

## What ClickHouse does

- MergeTree stores sorted, immutable parts, compresses column streams in blocks,
  keeps sparse marks, and merges parts in the background. Reads can use marks to
  locate compressed blocks and avoid reading unneeded columns and ranges.
- It combines type-aware transforms (dictionary, delta and Gorilla among others)
  with general-purpose compression. Sort order, cardinality and data shape have a
  major effect; the choice of LZ4 versus ZSTD trades speed against space.
- Processing columns in batches and applying filters and aggregation close to
  scans reduce work beyond the savings from compression. These mechanisms have
  different effects and should be measured separately.

Sources: [ClickHouse columnar storage](https://clickhouse.com/resources/engineering/what-is-columnar-storage),
[MergeTree wide-part writer source](https://github.com/ClickHouse/ClickHouse/blob/master/src/Storages/MergeTree/MergeTreeDataPartWriterWide.cpp),
and [ClickHouse codec guidance](https://clickhouse.com/blog/optimize-clickhouse-codecs-compression-schema).

## RedDB today

The repository already adopted several of these ideas in the time-series path.
[RDCC](../engine/columnar-chunk-format.md) has column streams, semantic codecs,
min/max marks and bloom indexes. The current selector uses LZ4 after DoubleDelta
for timestamps and XOR for gauges; ZSTD streams remain readable. See
[`select_codecs`](../../crates/reddb-server/src/storage/unified/segment_codec.rs)
and [`seal_columnar`](../../crates/reddb-server/src/storage/timeseries/chunk.rs).
General collections do not automatically acquire this layout. The
[execution documentation](../engine/columnar-execution.md) says general SQL
batch routing remains planned; do not assume batch primitives imply an end-to-end
vectorized SQL path.

There is a specific gap between skip metadata and physical decode: the range
reader calls `read_column_block` first, and that function decodes every column
stream before the range reader examines granule metadata. Bloom equality follows
the same pattern. The index reduces row comparisons, but does not currently
avoid decompression of rejected granules. See
[`query_column_block_range`](../../crates/reddb-server/src/storage/timeseries/chunk.rs)
and [`read_column_block_filtered`](../../crates/reddb-server/src/storage/unified/column_block.rs).
The RDCC v1 writer compresses one stream per column, and the reader verifies a
whole-block CRC, so row-level pruning does not imply fewer bytes read from disk.

The prior [columnar-read benchmark](2026-06-03-columnar-read.md) compares two
decoders of the same columnar block on synthetic time-series data; it is not a
row-storage-versus-columnar-storage disk benchmark, a general SQL benchmark, or
a ClickHouse comparison. Its 50K-row result reports 851 µs for batch decode and
950 µs for row-shaped decode in that run. Do not extrapolate that result to full
query or storage improvements.

[PRD #850](https://github.com/reddb-io/reddb/issues/850) deliberately limits
columnar storage to Metrics/TimeSeries and defers a standalone merge engine.
[ADR 0069](../../.red/adr/0069-columnar-analytics-projection.md) describes a
derived columnar projection alongside row storage, while the RDCC document and
PRD describe the sealed chunk itself as the columnar unit. Resolve the exact
current ownership and accounting from runtime and recovery code before making
claims about duplicate copies or choosing a storage change; these documents
express different architectural decisions.

## Does this need another collection model?

Recommendation: treat analytical storage and execution as capabilities of the
existing collection models, not as a new logical collection model. Analytics
v0 already describes metrics over ordinary collections and does not define an
analytics collection or a second raw-data write API
([ontology](../data-models/analytics.md)). A log, a time-series point, and a
business event have different logical shapes, but can share physical components
such as column encoding, pruning and batch aggregation when their contracts
allow it. Whether to expose workload-specific defaults is a product decision;
it does not require changing the collection model.

| Existing workload | Best fit and evidence | Work still needed |
| --- | --- | --- |
| TimeSeries / hypertable | The runtime materializes `(timestamp, value)` from the entity store, writes a two-column RDCC block, and records it on the chunk. The checkpoint path calls the sealer; tests exercise the read bridge. | Preserve the full series identity and attributes when extending the physical path; prove public SQL, restart, retention, snapshot and tenant behavior end to end. Avoid assuming the existing two-column block covers arbitrary hypertable fields. |
| Logs | The public log collection path uses `LogCollection` and `UnifiedStore`; structured events can also use existing tables or documents. | Define and verify a durable, queryable event representation and select useful text and field encodings. The separate `LogPipeline` helper currently routes timestamps and retains recent `LogLine`s in a bounded in-memory buffer; it is not evidence of durable log payload storage. |
| Analytics over tables / documents | Analytics source profiles name existing collections. `ColumnarProjection` demonstrates a derived append-only projection with snapshot concepts. | Connect and validate the real ingestion, checkpoint, planner and query path; support types and shapes needed by the chosen workload. Mutable collections additionally need update/delete and snapshot visibility handling. |

Evidence: [`seal_hypertable_chunks_internal`](../../crates/reddb-server/src/runtime/impl_timeseries.rs),
[`LogCollection`](../../crates/reddb-server/src/log/store.rs),
[`LogPipeline`](../../crates/reddb-server/src/storage/timeseries/log_pipeline.rs),
and [`ColumnarProjection`](../../crates/reddb-server/src/storage/columnar_projection.rs).
The latter explicitly identifies itself as an in-process tracer and says its
checkpoint and query-executor integration is follow-up work. At the studied
revision, searches for `emit_at_checkpoint`, `read_bridge_points`, and
`column_batch_from_block` found the respective implementations and tests (and
benchmarks for batch decode), but no public SQL execution call site for those
specific helpers. This does not prove no other SQL path exists; it does mean
these helpers alone do not establish a production SQL columnar pipeline.

The hypertable seal function reads existing entity rows to build the RDCC block
and writes a column block page. It does not itself delete the source rows. This
is concrete evidence that a compact block's size is not necessarily the total
storage footprint; audit the complete lifecycle before concluding how many
copies remain. Similarly, function implementations and module-level tests do
not by themselves establish that every documented SQL function is wired through
all public interfaces.

The HTTP log append path previously returned success when a storage write
failed. A regression test reproduced this with an inaccessible embedded WAL;
the handler now waits for the write result and returns an error on failure. A
second test verifies that an acknowledged entry survives closing and reopening
a paged store. See [`handle_log_append`](../../crates/reddb-server/src/server/handlers_log.rs).
The older public buffered `LogCollection::append` API still discards errors in
`flush_buffer`; it needs a separate compatibility decision before it can offer
reliable error reporting. A failed storage operation can also have an uncertain
outcome, so callers must not assume an error proves that no record was written.

ClickHouse's physical sort key is an optimization key, not a uniqueness
constraint ([ClickHouse primary-key guide](https://clickhouse.com/blog/a-simple-guide-to-clickhouse-query-optimization-part-1)).
If RedDB introduces sorted analytical blocks, that sort order must not silently
change the existing collection's key, duplicate, ordering, snapshot, tenant or
row-level-security semantics. Existing append-only contracts are the safest
first scope; a mutable model needs a separate visibility design. Compare a
derived projection's extra disk and rebuild costs with a durable-columnar design
before changing the source of truth or the recovery contract.

## Recommended next steps

1. **Establish an end-to-end baseline before choosing a format.** Use the same
   deterministic data and schema for RedDB and ClickHouse: time-series gauges,
   high/low-cardinality event attributes and representative text or token-usage
   events. Report row count, logical input bytes, cardinality, ordering, nulls,
   token-count fields versus actual text, batch size, hardware, versions,
   configuration and durability. Include compressible and less-compressible
   distributions. Verify exact query results and round-trip after restart.
2. **Measure physical footprint and work, not just compressed payload.** Capture
   column blocks, indexes, row data or other copies, WAL, manifests and total
   allocated filesystem bytes separately, at reproducible ingest, checkpoint and
   retention/compaction points. State which files and replicas are included.
   Measure ingestion throughput, peak memory, CPU, p50/p95 query latency and
   bytes read/decompressed for full scans, selective time ranges, aggregation
   and narrow projection. Compare equivalent durability, data, query results and
   cache state. No performance or compression target is implied by this note.
3. **Prototype block-level pruning only if the baseline justifies it.** Evaluate
   independently decodable per-granule or multi-granule frames with offsets and
   checksums, while preserving v1 reads, bounded allocation, integrity checks,
   tenant isolation, MVCC/snapshot behavior, WAL ordering and recovery. Measure
   the cost of extra metadata and smaller compression windows, not only selective
   query wins. This is a format and lifecycle design, not a codec switch.
4. **Measure the gap to useful SQL analytics.** Trace the actual planner and
   runtime path for simple aggregate/filter queries, including RLS and mixed
   row/columnar chunks, then evaluate a narrow batch route only when it preserves
   existing query semantics. Generic tables/documents and MergeTree-like
   compaction require separate product and architecture decisions.

This note is a source review and benchmark proposal; no comparative benchmark,
performance gain, or storage-size claim was measured in this investigation.
