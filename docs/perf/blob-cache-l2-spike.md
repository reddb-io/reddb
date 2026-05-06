# Blob Cache L2 Spike

Date: 2026-05-06

## Question

Can Blob Cache L2 reuse an existing ordered storage primitive, or does it need a new ordered store before durable cache wiring starts?

## Findings

### Ordered metadata

RedDB already has a page-backed B+ tree in `crates/reddb-server/src/storage/engine/btree.rs` and `crates/reddb-server/src/storage/engine/btree/impl.rs`.

Useful properties:

- Leaf pages are linked for range scans.
- Keys and values are byte slices.
- Public operations include `get`, `insert`, `delete`, `cursor_first`, `cursor_seek`, and `range`.
- It is already backed by `Pager`, so it participates in the native paged storage layer.

Constraints:

- Inline value size is capped by `MAX_VALUE_SIZE` (`1024` bytes), so Blob Cache metadata must stay compact and store blob bytes out-of-line.
- Root page ownership needs to be added to `PhysicalFileHeader` or a native metadata summary page; there is no current dedicated Blob Cache root slot.

Decision: reuse the existing B+ tree for L2 metadata and secondary indexes. Do not introduce a new ordered store for #145.

### Native blob bytes

`UnifiedStore` already has native blob-chain helpers in `storage/unified/store/impl_native_c.rs` using `NATIVE_BLOB_MAGIC` (`RDBL`):

- `write_native_blob_chain(payload, existing_root)` writes page-aligned chunk chains.
- `read_native_blob_chain(root_page)` reads the chain back.
- The vector artifact store already uses this path for out-of-row bytes.

Constraints:

- Blob chains are addressed by root page and linked-list traversal; they are not independently range-scannable.
- Sweeps need metadata/index scans to discover root pages, then reclaim chains through pager/free-list work.
- The helper is currently private for writes, so #145 should either expose a narrow storage-internal blob-chain API or move Blob Cache L2 into the same storage module boundary.

Decision: reuse the native blob-chain format for L2 bytes. Do not store Blob Cache bytes as normal JSON row values.

### Existing secondary indexes

The table/index subsystem has several specialized indexes, but they are shaped around collection rows and query planning. They are not a clean fit for cache metadata because Blob Cache needs:

- exact metadata key: `(namespace_hash, key_hash, key_bytes)`
- expiry index
- dependency/tag indexes
- tombstone/generation visibility

Decision: use separate B+ tree roots for Blob Cache metadata, expiry, dependency, and tag indexes rather than routing through table secondary indexes.

## Proposed #145 Shape

#145 can remain a single implementation slice if it stays storage-internal:

1. Add Blob Cache L2 roots to native physical metadata.
2. Add compact metadata records in a B+ tree keyed by `(namespace_hash, key_hash, key_bytes)`.
3. Store bytes in native blob chains and commit metadata last.
4. Add expiry and invalidation indexes as separate B+ trees.
5. Add `cache.blob.l2_bytes_max` with default `4 GiB`.
6. Add `CacheError::L2Full`, `reddb_cache_blob_l2_bytes_in_use{namespace}`, and `reddb_cache_blob_l2_full_rejections_total`.
7. Add test-only fault injection between blob-chain write and metadata commit.

## Crash Ordering

Required write ordering:

1. Write blob-chain pages.
2. Flush/sync enough for the pages to be durable.
3. Write metadata/index entries that make the key visible.
4. Commit/flush metadata.

If a process dies between steps 1 and 3, reopen must not return the entry because no metadata points at the blob chain. Startup or sweeper must reclaim orphan blob chains within one sweep cycle.

Invalidation ordering must write the tombstone/generation change before any path can rehydrate from L2. Readers must compare metadata generation/tombstone state before returning bytes.

## Decision

Proceed with #145 as a 1-2 week slice using existing primitives:

- B+ tree for ordered metadata and indexes.
- Native blob chains for bytes.
- New physical metadata roots for Blob Cache L2.

Do not split out a new ordered-store primitive. Split only if implementation discovers that B+ tree roots cannot be safely owned outside collection roots without broad physical-header churn.
