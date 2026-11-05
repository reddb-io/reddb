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
