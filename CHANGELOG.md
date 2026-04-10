# Changelog

## [Unreleased]

### New Data Structures (10 total)
- **Bloom Filter**: Per-segment probabilistic key testing, auto-populated on insert, bloom pruning in query executor
- **Hash Index**: O(1) exact-match lookups via `CREATE INDEX ... USING HASH`
- **Bitmap Index**: Roaring bitmap for low-cardinality columns via `CREATE INDEX ... USING BITMAP`
- **R-Tree Spatial Index**: Radius/bbox/nearest-K geo queries via `SEARCH SPATIAL` and `CREATE INDEX ... USING RTREE`
- **Skip List + Memtable**: Write buffer in GrowingSegment, sorted drain on seal
- **HyperLogLog**: Approximate distinct counting (`CREATE HLL`, `HLL ADD/COUNT/MERGE`)
- **Count-Min Sketch**: Frequency estimation (`CREATE SKETCH`, `SKETCH ADD/COUNT`)
- **Cuckoo Filter**: Membership testing with deletion (`CREATE FILTER`, `FILTER ADD/CHECK/DELETE`)
- **Time-Series**: Chunked storage with delta-of-delta timestamps, Gorilla XOR compression, retention policies, time-bucket aggregation (`CREATE TIMESERIES`, retention, downsampling)
- **Queue / Deque**: FIFO/LIFO/Priority message queue with consumer groups (`CREATE QUEUE`, `QUEUE PUSH/POP/PEEK/LEN/PURGE/ACK/NACK`)

### Query Language Extensions
- `CREATE INDEX [UNIQUE] name ON table (cols) USING HASH|BTREE|BITMAP|RTREE`
- `DROP INDEX [IF EXISTS] name ON table`
- `SEARCH SPATIAL RADIUS lat lon km COLLECTION col COLUMN col [LIMIT n]`
- `SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col`
- `SEARCH SPATIAL NEAREST lat lon K n COLLECTION col COLUMN col`
- `CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]`
- `DROP TIMESERIES [IF EXISTS] name`
- `CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]`
- `DROP QUEUE [IF EXISTS] name`
- `QUEUE PUSH|POP|PEEK|LEN|PURGE|GROUP CREATE|READ|ACK|NACK`
- `CREATE/DROP HLL|SKETCH|FILTER` + `HLL ADD/COUNT/MERGE` + `SKETCH ADD/COUNT` + `FILTER ADD/CHECK/DELETE`
- JSON inline literals: `{key: value}` without quotes in VALUES and QUEUE PUSH

### Deep Integration
- **IndexStore**: Unified manager for Hash/Bitmap/Spatial indices in RuntimeInner
- **IndexSelectionPass**: Query optimizer analyzes WHERE and recommends Hash/BTree/Bitmap automatically
- **Bloom filter pruning**: Executor skips segments when bloom says key is absent
- **Spatial search**: Functional radius/bbox/nearest with Haversine distance on GeoPoint and lat/lon fields
- **Memtable**: Write buffer integrated in GrowingSegment lifecycle
- **red_config**: All new features configurable via `SET CONFIG red.indexes.*`, `red.memtable.*`, `red.probabilistic.*`, `red.timeseries.*`, `red.queue.*`
- **ProbabilisticCommand dispatch**: HLL/CMS/Cuckoo fully functional end-to-end via SQL

### Dependencies
- Added `roaring = "0.10"` (Bitmap Index)
- Added `rstar = "0.12"` (R-Tree Spatial Index)

### Previous
- Initial public release preparation.
- Added multi-model embedded/server/serverless documentation and packaging pipeline.
- Added unified crate publishing workflow for crates.io.
