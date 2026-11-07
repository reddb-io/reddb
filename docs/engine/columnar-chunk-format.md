# RDCC — Columnar Chunk Format

When a time-series / hypertable collection crosses the configured automatic
projection size floor, a sealed chunk is written to disk in a column-major
envelope called **`RDCC`** (RedDB Columnar Chunk) instead of the row form.
This page documents that on-disk byte contract. It is engine-facing: it
describes exactly what the writer emits so the format stays auditable and a
reader can be implemented from this page alone.

> Source of truth:
> [`storage/unified/column_block.rs`](../../crates/reddb-server/src/storage/unified/column_block.rs)
> (the envelope) and
> [`storage/unified/segment_codec.rs`](../../crates/reddb-server/src/storage/unified/segment_codec.rs)
> (the per-column codec pipeline). The chunk-level writer/reader live in
> [`storage/timeseries/chunk.rs`](../../crates/reddb-server/src/storage/timeseries/chunk.rs).

An `RDCC` block is **not** a parallel object alongside the chunk — the
sealed chunk *is* the columnar segment. PRD #850 explicitly rejects a
standalone "columnar segment" as a storage-bloat vector. The block is
emitted as a [`PageType::ColumnBlock`](pager.md) page, and the sealed
chunk records its `PageLocation` in `ChunkMeta.columnar_page`.

## Why columnar at all

A sealed chunk is immutable history. Analytical scans over it read one or
two columns out of many and run min/max/sum/count reductions. Storing the
chunk column-major lets the engine:

- compress each column with a codec matched to its shape (timestamps vs.
  float gauges compress very differently);
- skip whole **granules** (row ranges) that cannot match a predicate,
  using a sparse min/max index for ranges and a per-granule bloom for
  equality; a range that excludes every granule skips column decoding;
- decode only the columns a query references (projection pushdown).

General-purpose collections stay on the row engine, and in-scope collections
can opt out with `NO COLUMNAR`. See
[When to use columnar vs row](../data-models/timeseries.md#when-to-use-columnar-vs-row).

## Block layout

All multi-byte integers are **little-endian**. The block opens and closes
with the magic `b"RDCC"`, so a truncated or corrupt block is caught at both
ends, and an interior CRC32 guards the payload.

```text
Header (52 bytes)
  magic            b"RDCC"   (4)
  format_version   u16  = 1
  flags            u16  = 0           reserved
  chunk_id         u64
  schema_ref       u64                catalog schema id the column_ids resolve against
  row_count        u64
  column_count     u32
  min_ts_ns        u64                mirrors ChunkMeta → block is self-describing
  max_ts_ns        u64

Column directory   (column_count entries, 54 bytes each, at offset 52)
  column_id            u32
  logical_type         u8             value type tag (DataType::to_byte)
  codec                u8             leading (semantic) ColumnCodec tag
  stream_offset        u64            byte offset of this column's stream
  stream_len           u64
  granule_index_off    u64            offset of this column's granule index (0 = none)
  granule_index_len    u64            length of the granule index blob (0 = none)
  bloom_off            u64            offset of this column's granule bloom (0 = none)
  bloom_len            u64            length of the granule bloom blob (0 = none)

Column streams        column_count codec-pipeline runs, back-to-back
Granule indexes       per-column min/max blobs, back-to-back
Granule blooms        per-column bloom blobs, back-to-back

Footer (24 bytes)
  col_directory_off    u64
  col_directory_len    u64
  crc32                u32            over every byte before the footer
  magic_tail           b"RDCC"  (4)
```

The header is fixed at **52 bytes**, each directory entry is **54 bytes**,
and the footer is **24 bytes**. `format_version` is `1`; the directory
carries reserved offset/length fields (granule index, bloom) so additive
extensions land without bumping the version — a reader rejects any version
it does not understand.

### CRC coverage

The footer's `crc32` is computed over **every byte before the footer** —
header, directory, all column streams, all granule indexes, and all granule
blooms. (The inline code comment abbreviates this as "header+directory+
streams"; the writer in fact CRCs the whole pre-footer buffer.) On read the
CRC is always verified before any column is decoded — integrity is not
negotiable, even under projection pushdown.

## Column directory

One 54-byte entry per column, in write order. `logical_type` is the
`DataType::to_byte()` tag; `codec` is the **leading** codec of the column's
pipeline (see below), recorded purely as self-describing bookkeeping — the
reader actually decodes from each stream's own header, never from this byte.
`stream_offset`/`stream_len` locate the column's compressed bytes. The
`granule_index_*` and `bloom_*` pairs locate the column's skip indexes, or
are both zero when the column has none (e.g. variable-width columns).

A time-series chunk writes exactly two columns:

| `column_id` | meaning | `logical_type` | semantics → codec |
|:-----------:|:--------|:---------------|:------------------|
| `0` | timestamp (ns) | `UnsignedInteger` | `Timestamp` → DoubleDelta + LZ4 |
| `1` | value | `Float` | `Gauge` → Xor + LZ4 |

## Per-column codec pipeline

Each column stream is one `segment_codec` pipeline output. The codec chain
is chosen **per column from its semantics**, not globally. The directory's
`codec` byte records the leading (semantic) codec; the full chain is
self-described inside the stream header so reads never consult the schema.

Codec tags (one byte, stored in stream + directory headers):

| Tag | Codec | Best on |
|:---:|:------|:--------|
| 0 | `None` | small / already-compressed data |
| 1 | `Lz4` | generic fast compression |
| 2 | `Zstd { level }` | generic, higher ratio than LZ4 |
| 3 | `Delta` | monotonic / near-monotonic ints (counters) |
| 4 | `DoubleDelta` | regular-interval timestamps |
| 5 | `Dict` | low-cardinality strings / enums |
| 6 | `Xor` | floating-point gauges (Gorilla XOR) |

Semantics drive selection. The selected semantic codecs are chained with LZ4,
which favors read speed. The self-describing streams also allow older
ZSTD-compressed data to be read:

| Column semantics | Pipeline |
|:-----------------|:---------|
| `Timestamp` | `DoubleDelta` → `LZ4` |
| `Gauge` | `Xor` → `LZ4` |
| `Counter` | `Delta` → `LZ4` |
| `LowCardinality` | `Dict` → `LZ4` |
| `Generic` | `LZ4` |

The stream header records the chain so decode just runs the codecs in
reverse. Round-trips are lossless for every codec/type pair the selector
emits, including special floats (NaN / ±inf / signed zero survive the XOR
path bit-for-bit).

## Sparse granule index (range pruning)

A column's rows are tiled into fixed-size **granules** — one min/max mark
per `granule_size` rows. The default stride is **8192 rows**
(`DEFAULT_GRANULE_SIZE`), configurable per seal. This is the chunk's
BRIN-style skip index made granular: the reader keeps only granules whose
`[min, max]` interval can intersect a range predicate and examines rows in
those granules. The preliminary index check borrows min/max entries directly
from the frame without allocating a second copy. If the complete timestamp
index excludes all granules, the range reader returns without decoding either
column. When any granule may
match, it decodes both complete column streams before examining surviving rows;
it cannot decompress individual granules independently in RDCC v1. The
whole-block CRC and directory are validated before pruning; skipped streams
are not decoded, so malformed compressed data with a valid CRC is detected
only when a query reads that stream.

```text
Granule index blob (per indexed column)
  granule_size_rows    u32            rows per mark (the last mark may be shorter)
  value_width          u32            bytes per min/max value (8 for u64/f64)
  granule_count        u32
  per granule:  min[value_width]  max[value_width]   raw column-encoded bytes
```

`min`/`max` are stored in the column's own little-endian encoding, but the
*ordering* used to compute them is type-aware (`i64` / `u64` / `f64` via
`f64::total_cmp`) because raw byte order is wrong for signed ints and floats.
The index covers only fixed-width numeric columns; variable-width streams
(e.g. dictionary-coded text) get a zero-length slice and no index.

**Soundness:** a granule survives whenever `granule_min <= end &&
granule_max >= start`, i.e. exactly when it *could* hold a matching row, so
pruning never drops a match regardless of where granule boundaries fall.
When a block carries no index, every row is scanned (still correct). Pruning
is observable — a selective scan reports `granules_scanned < granules_total`.

## Per-granule bloom skip index (equality pruning)

Where min/max serves ranges, a **split-block bloom filter** per granule —
tiled on the same boundaries — serves equality / point predicates. Each
value in a granule is folded through `hash_bytes_u32` and represented; an
equality probe keeps only granules whose bloom *may* contain the target.

```text
Granule bloom blob (per indexed column)
  granule_size_rows    u32            rows per bloom (the last bloom may cover fewer)
  granule_count        u32
  per granule:  num_blocks u32        then num_blocks × 32 bytes of bloom words
```

The writer builds the bloom once using row-count sizing, then tries folding
its block count in half. Corresponding blocks are OR-merged under the reduced
routing mask, preserving every inserted bit. A fold is accepted only while
each merged word remains at most half full. This conservative occupancy check
avoids saturating the filter. Compaction scans bloom blocks and never rehashes
the column values; the serialized format stays unchanged. The actual space
saving depends on the data.

**Soundness:** a split-block bloom never reports a false negative, so a
granule that actually holds the target always probes true and survives —
equality pruning over-includes (false positives) but never under-includes.
Survivors are still compared exactly per row, on the raw 8-byte encoding the
bloom was built from.

## Projection pushdown

`read_column_block_projected(bytes, want)` decodes only the columns whose
`column_id` appears in `want`. Columns outside the set are skipped *before*
the expensive decode / granule / bloom parse, so a scan referencing a subset
of columns decodes only those columns. The whole-block CRC is still verified
regardless, so it must process the compressed bytes of all columns.

## Read path

The chunk-level reader transposes the two column streams back into
`(timestamp_ns, value)` rows:

- `points_from_column_block(bytes)` — full row decode, every row.
- `column_batch_from_block(bytes, projection)` — typed zero-copy batch decode
  into a [`ColumnBatch`](columnar-execution.md#columnar-read-decode). Numeric
  columns decode straight into an aligned word buffer that is reinterpreted in
  place as `i64` / `f64` without a copy (committed in #962). This is the path a
  vectorized scan consumes.
- `query_column_block_range(bytes, start_ns, end_ns)` — range scan that
  prunes via the timestamp column's granule index.
- `query_column_block_value_eq(bytes, target)` — point query that prunes via
  the value column's granule bloom.

The range and value-eq scans return a `PrunedColumnScan` carrying
`granules_total` and `granules_scanned`, which is how pruning is made
measurable end to end.

Activation note: this read path is live for the time-series / hypertable read
bridge on automatically projected chunks. Routing general `SELECT` execution
through the batch decode is not wired yet — see
[Columnar Batch Execution](columnar-execution.md#columnar-read-decode).

## See also

- [Columnar Batch Execution](columnar-execution.md) — the vectorized
  `ColumnBatch` operators that consume decoded columns.
- [`.rdb` File Format Specification](file-format.md) — the page-based
  container an `RDCC` block lives inside (`PageType::ColumnBlock`).
- [Time-Series → Storage Architecture](../data-models/timeseries.md#storage-architecture)
  — how automatic projection and `NO COLUMNAR` work.
- PRD #850 (analytics storage engine); activation shipped in #911.
