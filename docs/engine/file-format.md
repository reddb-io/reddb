# `.rdb` File Format Specification

RedDB persists data in `.rdb` files using a page-based binary format. This document describes the complete physical layout derived from source code.

---

## File Overview

```
┌──────────────────────────────────────────────────────────────┐
│ Page 0: Database Header                                       │
│   Magic: "RDDB" │ Version │ Page size │ Page count │ Freelist │
├──────────────────────────────────────────────────────────────┤
│ Page 1: Metadata (Collections Registry + Cross-References)    │
│   Magic: "RDM2" │ Collections │ B-tree roots │ Cross-refs    │
├──────────────────────────────────────────────────────────────┤
│ Page 2..N: B-Tree Pages (entities, indexes)                   │
│   Interior nodes │ Leaf nodes │ Overflow pages                │
├──────────────────────────────────────────────────────────────┤
│ Separate file: WAL (Write-Ahead Log)                          │
│   Magic: "RDBW" │ Begin/Commit/Rollback/PageWrite/Checkpoint │
└──────────────────────────────────────────────────────────────┘
```

---

## 1. Page Structure (4096 bytes)

Every page is exactly 4096 bytes. The first 32 bytes are a fixed header; the remaining 4064 bytes hold content.

### Page Header (32 bytes)

| Offset | Size | Type | Field | Description |
|:-------|:-----|:-----|:------|:------------|
| 0 | 1 | u8 | `page_type` | See [Page Types](#page-types) |
| 1 | 1 | u8 | `flags` | Bitfield: Dirty `0x01`, Locked `0x02`, Loaded `0x04`, Pinned `0x08`, Encrypted `0x10` |
| 2 | 2 | u16 LE | `cell_count` | Number of cells on this page |
| 4 | 2 | u16 LE | `free_start` | First free byte in cell pointer array |
| 6 | 2 | u16 LE | `free_end` | First free byte before cell content area |
| 8 | 4 | u32 LE | `page_id` | Unique page identifier |
| 12 | 4 | u32 LE | `parent_id` | Parent page (B-tree navigation) |
| 16 | 4 | u32 LE | `right_child` | Right child page (B-tree interior only) |
| 20 | 8 | u64 LE | `lsn` | Log Sequence Number (WAL byte offset) |
| 28 | 4 | u32 LE | `checksum` | CRC32 of entire page content |

### Page Content Layout (4064 bytes)

```
Offset 32                                              Offset 4095
│ Cell Pointers (grows →)    │  Free Space  │ Cell Data (← grows) │
│ [ptr1][ptr2][ptr3]...      │              │ ...data3|data2|data1 │
```

- **Cell pointers**: 2-byte (`u16`) offsets into the cell content area
- **Free space**: unallocated gap between pointers and data
- **Cell data**: variable-length records, packed from the page bottom upward

### Page Types

| Value | Name | Description |
|:------|:-----|:------------|
| 0 | `Free` | Available for allocation |
| 1 | `BTreeLeaf` | B-tree leaf (key-value pairs) |
| 2 | `BTreeInterior` | B-tree interior (keys + child pointers) |
| 3 | `Overflow` | Continuation of large values |
| 4 | `Vector` | Dense vector storage |
| 5 | `FreelistTrunk` | Tracks free pages |
| 6 | `Header` | Database header (page 0) |
| 7 | `GraphNode` | Packed graph node records |
| 8 | `GraphEdge` | Packed graph edge records |
| 9 | `GraphAdjacency` | Outgoing edges per node |
| 10 | `GraphMeta` | Graph statistics and index roots |
| 11 | `NativeMeta` | Engine-published auxiliary state |
| 12 | `Vault` | Encrypted auth vault (users, API keys) |

---

## 2. Database Header (Page 0)

Page 0 identifies the database and tracks physical metadata.

| Offset | Size | Type | Field | Value |
|:-------|:-----|:-----|:------|:------|
| 0-31 | 32 | — | Page header | `page_type=6` (Header), `page_id=0` |
| 32 | 4 | `[u8;4]` | `magic` | `RDDB` (`0x52 0x44 0x44 0x42`) |
| 36 | 4 | u32 LE | `version` | `0x00010000` (format 1.0.0) |
| 40 | 4 | u32 LE | `page_size` | `4096` (always) |
| 44 | 4 | u32 LE | `page_count` | Total pages in file |
| 48 | 4 | u32 LE | `freelist_head` | First freelist trunk page (0 = none) |
| 52-4095 | — | — | Reserved | Zeros |

---

## 3. Metadata Page (Page 1)

Page 1 contains the **collection registry** and **cross-references**. Format version 2+ uses a magic header for forward compatibility.

### Layout

```
Offset 32 (after page header):
┌─────────────────────────────────────────────────────────────┐
│ "RDM2" (4 bytes)           │ format_version (u32 LE)        │
├─────────────────────────────────────────────────────────────┤
│ collection_count (u32 LE)                                    │
├─────────────────────────────────────────────────────────────┤
│ Collection 1: [name_len: u32 LE][name: UTF-8][root_page: u32 LE] │
│ Collection 2: ...                                            │
├─────────────────────────────────────────────────────────────┤
│ cross_ref_count (u32 LE)                   (V2+ only)       │
│ CrossRef 1: [source: u64][target: u64][type: u8]            │
│             [col_len: u32][collection: UTF-8]                │
│ CrossRef 2: ...                                              │
└─────────────────────────────────────────────────────────────┘
```

Each collection entry maps a name to a B-tree root page where its entities are stored. The metadata page uses **fixed-width integers** (not varints).

---

## 4. Freelist

Free pages are tracked via linked trunk pages, each containing an array of available page IDs.

### Freelist Trunk Page Layout

| Offset | Size | Type | Field |
|:-------|:-----|:-----|:------|
| 0-31 | 32 | — | Page header (`page_type=5`) |
| 32 | 4 | u32 LE | `next_trunk` — next trunk page ID (0 = last) |
| 36 | 4 | u32 LE | `count` — number of free page IDs in this trunk |
| 40-4095 | variable | `[u32 LE]` | Array of free page IDs |

**Capacity**: `(4096 - 32 - 8) / 4 = 1014` free page IDs per trunk.

Trunks form a singly-linked list. The `freelist_head` in page 0 points to the first trunk.

---

## 5. B+ Tree Organization

Each collection has its own B+ tree rooted at the page stored in the metadata page.

### Properties

- All data in **leaf nodes** (interior nodes store only keys + child pointers)
- Leaf nodes form a **doubly-linked list** for range scans
- Max key size: **1024 bytes**
- Max inline value size: **1024 bytes**
- Minimum fill factor: **25%** before merge

### Leaf Page Layout

| Offset | Size | Type | Field |
|:-------|:-----|:-----|:------|
| 0-31 | 32 | — | Page header (`page_type=1`) |
| 32 | 4 | u32 LE | `prev_leaf` — previous leaf page (0 = none) |
| 36 | 4 | u32 LE | `next_leaf` — next leaf page (0 = none) |
| 40+ | variable | — | Cell data |

**Leaf cell format**:
```
[key_len: u16 LE][val_len: u16 LE][key: bytes][value: bytes]
```

### Interior Page Layout

| Offset | Size | Type | Field |
|:-------|:-----|:-----|:------|
| 0-31 | 32 | — | Page header (`page_type=2`, `right_child` in header) |
| 32+ | variable | — | Cell data |

**Interior cell format**:
```
[key_len: u16 LE][key: bytes][child_page_id: u32 LE]
```

The rightmost child pointer lives in the page header's `right_child` field.

### Tree Shape

```
            [Interior Page]
           /       |        \
    [Leaf Page] [Leaf Page] [Leaf Page]
     e1,e2,e3    e4,e5,e6    e7,e8,e9
         ←prev     ←→         next→
```

Keys are 8-byte entity IDs (little-endian `u64`). Values are serialized entities (see [Entity Binary Format](#7-entity-binary-format)).

---

## 6. Varint Encoding (LEB128)

Variable-length integers are used extensively in entity and binary store serialization.

### Unsigned Varint (varu32 / varu64)

Each byte stores 7 data bits. The high bit (`0x80`) indicates more bytes follow:

```
value < 128:     [0xxxxxxx]                           → 1 byte
value < 16384:   [1xxxxxxx] [0xxxxxxx]                → 2 bytes
value < 2097152: [1xxxxxxx] [1xxxxxxx] [0xxxxxxx]     → 3 bytes
...
```

- **varu32**: 1-5 bytes, overflow at shift >= 35
- **varu64**: 1-10 bytes, overflow at shift >= 70

### Signed Varint (vari32 / vari64)

Uses zigzag encoding to map signed integers to unsigned:
```
zigzag = (value << 1) ^ (value >> 31)   // for i32
zigzag = (value << 1) ^ (value >> 63)   // for i64
```

Then encoded as unsigned varint. Small absolute values use fewer bytes regardless of sign.

---

## 7. Entity Binary Format

Entities are the fundamental data units stored in B-tree leaf cells and in binary store files. All multi-byte integers are little-endian unless using varint encoding.

### Entity Layout

```
[id: varu64]
[kind_type: u8]
[kind_data: ...]          ← varies by kind_type
[data_type: u8]
[data: ...]               ← varies by data_type
[created_at: varu64]      ← timestamp (nanoseconds)
[updated_at: varu64]      ← timestamp (nanoseconds)
[embedding_count: varu32]
[embeddings: ...]         ← repeated EmbeddingSlot
[cross_ref_count: varu32]
[cross_refs: ...]         ← repeated CrossRef
[sequence_id: varu64]
```

### EntityKind Types

| Byte | Kind | Payload |
|:-----|:-----|:--------|
| 0 | `TableRow` | `[table_len: varu32][table: UTF-8][row_id: varu64]` |
| 1 | `GraphNode` | `[label_len: varu32][label: UTF-8][type_len: varu32][node_type: UTF-8]` |
| 2 | `GraphEdge` | `[label_len: varu32][label: UTF-8][from_len: varu32][from_node: UTF-8][to_len: varu32][to_node: UTF-8][weight: u32 LE]` |
| 3 | `Vector` | `[col_len: varu32][collection: UTF-8]` |

### EntityData Types

| Byte | Data | Payload |
|:-----|:-----|:--------|
| 0 | `Row` (positional) | `[col_count: varu32][columns: Value...]` |
| 1 | `Node` | `[prop_count: varu32][properties: (key_len: varu32, key: UTF-8, Value)...]` |
| 2 | `Edge` | `[weight: f32 LE][prop_count: varu32][properties: (key_len: varu32, key: UTF-8, Value)...]` |
| 3 | `Vector` | `[dim: varu32][elements: f32 LE...]` |
| 4 | `TimeSeries` | `[metric_len: varu32][metric: UTF-8][timestamp_ns: varu64][value: f32 LE]` |
| 5 | `QueueMessage` | `[payload: Value][enqueued_at_ns: varu64][attempts: u32 LE]` |
| 6 | `Row` (named) | `[field_count: varu32][fields: (key_len: varu32, key: UTF-8, Value)...]` |

### Embedding Slot Format

```
[name_len: varu32][name: UTF-8]
[dim: varu32][vector: f32 LE × dim]
[model_len: varu32][model: UTF-8]
```

### Cross-Reference Format

**V1**:
```
[source_id: varu64][target_id: varu64][ref_type: u8]
```

**V2** (current):
```
[source_id: varu64][target_id: varu64][ref_type: u8]
[col_len: varu32][target_collection: UTF-8]
[weight: f32 LE][created_at: varu64]
```

### RefType Byte Values

| Byte | Type | Description |
|:-----|:-----|:------------|
| 0 | `RowToNode` | Table row represents a graph node |
| 1 | `RowToEdge` | Table row represents a graph edge |
| 2 | `NodeToRow` | Node links back to source row |
| 3 | `RowToVector` | Table row has embeddings |
| 4 | `VectorToRow` | Vector search → source row |
| 5 | `NodeToVector` | Node has embeddings |
| 6 | `EdgeToVector` | Edge has embeddings |
| 7 | `VectorToNode` | Vector search → source node |
| 8 | `SimilarTo` | Semantic similarity link |
| 9 | `RelatedTo` | General relation |
| 10 | `DerivesFrom` | Derivation chain |
| 11 | `Mentions` | Entity mentions another |
| 12 | `Contains` | Container relationship |
| 13 | `DependsOn` | Dependency link |

---

## 8. Value Type Tags (48 types)

Every serialized `Value` is prefixed by a single type byte. All integers are little-endian. Variable-length fields use varint-prefixed length.

### Scalar Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 0 | `Null` | 1 | `[tag]` |
| 1 | `Boolean` | 2 | `[tag][0x00 or 0x01]` |
| 2 | `Integer` | 9 | `[tag][i64 LE]` |
| 3 | `UnsignedInteger` | 9 | `[tag][u64 LE]` |
| 4 | `Float` | 9 | `[tag][f64 LE]` |
| 26 | `Decimal` | 9 | `[tag][i64 LE]` |
| 43 | `BigInt` | 9 | `[tag][i64 LE]` |
| 27 | `EnumValue` | 2 | `[tag][u8]` |

### Text & Binary

| Tag | Type | Encoding |
|:----|:-----|:---------|
| 5 | `Text` | `[tag][len: varu32][UTF-8 bytes]` |
| 6 | `Blob` | `[tag][len: varu32][raw bytes]` |
| 12 | `Json` | `[tag][len: varu32][raw bytes]` |

### Temporal Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 7 | `Timestamp` | 9 | `[tag][i64 LE]` (nanoseconds since epoch) |
| 8 | `Duration` | 9 | `[tag][i64 LE]` (nanoseconds) |
| 24 | `Date` | 5 | `[tag][i32 LE]` (days since epoch) |
| 25 | `Time` | 5 | `[tag][u32 LE]` (milliseconds since midnight) |
| 29 | `TimestampMs` | 9 | `[tag][i64 LE]` (milliseconds since epoch) |

### Network Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 9 | `IpAddr` | 6 or 18 | `[tag][version: u8 (4 or 6)][4 or 16 octets]` |
| 10 | `MacAddr` | 7 | `[tag][6 bytes]` |
| 23 | `Cidr` | 6 | `[tag][ip: u32 LE][prefix: u8]` |
| 30 | `Ipv4` | 5 | `[tag][u32 LE]` |
| 31 | `Ipv6` | 17 | `[tag][16 bytes]` |
| 32 | `Subnet` | 9 | `[tag][ip: u32 LE][mask: u32 LE]` |
| 33 | `Port` | 3 | `[tag][u16 LE]` |

### Geo Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 34 | `Latitude` | 5 | `[tag][i32 LE]` (encoded degrees) |
| 35 | `Longitude` | 5 | `[tag][i32 LE]` (encoded degrees) |
| 36 | `GeoPoint` | 9 | `[tag][lat: i32 LE][lon: i32 LE]` |

### Locale Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 37 | `Country2` | 3 | `[tag][2 ASCII chars]` (ISO 3166-1 alpha-2) |
| 38 | `Country3` | 4 | `[tag][3 ASCII chars]` (ISO 3166-1 alpha-3) |
| 39 | `Lang2` | 3 | `[tag][2 ASCII chars]` (ISO 639-1) |
| 40 | `Lang5` | 6 | `[tag][5 ASCII chars]` (BCP 47, e.g. `pt-BR`) |
| 41 | `Currency` | 4 | `[tag][3 ASCII chars]` (ISO 4217) |

### Color Types

| Tag | Type | Size | Encoding |
|:----|:-----|:-----|:---------|
| 18 | `Color` | 4 | `[tag][R][G][B]` |
| 42 | `ColorAlpha` | 5 | `[tag][R][G][B][A]` |

### Identity Types

| Tag | Type | Encoding |
|:----|:-----|:---------|
| 13 | `Uuid` | `[tag][16 bytes]` (128-bit UUID) |
| 19 | `Email` | `[tag][len: varu32][UTF-8]` |
| 20 | `Url` | `[tag][len: varu32][UTF-8]` |
| 21 | `Phone` | `[tag][u64 LE]` (numeric representation) |
| 22 | `Semver` | `[tag][u32 LE]` (packed version) |

### Vector & Array

| Tag | Type | Encoding |
|:----|:-----|:---------|
| 11 | `Vector` | `[tag][len: varu32][f32 LE × len]` |
| 28 | `Array` | `[tag][len: varu32][Value × len]` (recursive) |

### Reference Types

| Tag | Type | Encoding |
|:----|:-----|:---------|
| 14 | `NodeRef` | `[tag][len: varu32][node_id: UTF-8]` |
| 15 | `EdgeRef` | `[tag][len: varu32][edge_id: UTF-8]` |
| 16 | `VectorRef` | `[tag][col_len: varu32][collection: UTF-8][id: u64 LE]` |
| 17 | `RowRef` | `[tag][col_len: varu32][collection: UTF-8][id: u64 LE]` |
| 44 | `KeyRef` | `[tag][col_len: varu32][collection: UTF-8][key_len: varu32][key: UTF-8]` |
| 45 | `DocRef` | `[tag][col_len: varu32][collection: UTF-8][id: u64 LE]` |
| 46 | `TableRef` | `[tag][len: varu32][table_name: UTF-8]` |
| 47 | `PageRef` | `[tag][u32 LE]` (page ID) |

---

## 9. Write-Ahead Log (WAL)

Separate file alongside the `.rdb` database. Ensures crash recovery via write-ahead logging.

### WAL Header (8 bytes)

| Offset | Size | Type | Field | Value |
|:-------|:-----|:-----|:------|:------|
| 0 | 4 | `[u8;4]` | `magic` | `RDBW` (`0x52 0x44 0x42 0x57`) |
| 4 | 1 | u8 | `version` | `1` |
| 5 | 3 | `[u8;3]` | reserved | `0x00 0x00 0x00` |

### WAL Records

Records are appended sequentially after the header. Each record ends with a CRC32 checksum.

**Begin / Commit / Rollback** (13 bytes each):
```
[type: u8 (1/2/3)][tx_id: u64 LE][checksum: u32 LE]
```

**PageWrite** (21 + N bytes):
```
[type: u8 (4)][tx_id: u64 LE][page_id: u32 LE][data_len: u32 LE][data: N bytes][checksum: u32 LE]
```

**Checkpoint** (13 bytes):
```
[type: u8 (5)][lsn: u64 LE][checksum: u32 LE]
```

### Record Type Values

| Type | Value | Fields |
|:-----|:------|:-------|
| Begin | 1 | `tx_id` |
| Commit | 2 | `tx_id` |
| Rollback | 3 | `tx_id` |
| PageWrite | 4 | `tx_id`, `page_id`, `data_len`, `data` |
| Checkpoint | 5 | `lsn` |

### LSN (Log Sequence Number)

The LSN is the **byte offset** in the WAL file. After writing the 8-byte header, the first record starts at LSN 8. Each `append()` advances the LSN by the encoded record size.

### Checkpoint Process

1. Read all WAL records sequentially
2. Track transaction states (Begin → Commit/Rollback)
3. Collect PageWrite records for **committed** transactions only
4. Apply committed pages to database file in LSN order
5. `fsync` database to disk
6. Update checkpoint LSN in header
7. Truncate WAL (rewrite header, reset LSN to 8)

---

## 10. Binary Store Format (Non-Paged Mode)

For `save_to_file()` / `load_from_file()` — a simpler sequential format without page overhead.

### Layout

```
[magic: 4 bytes "RDST" (0x52 0x44 0x53 0x54)]
[version: u32 LE]                              ← 1 (V1) or 2 (V2)
[collection_count: varu32]
[for each collection:]
  [name_len: varu32][name: UTF-8]
  [entity_count: varu32]
  [for each entity:]
    [entity binary format]                     ← see section 7
[cross_ref_count: varu32]                      ← V2 only
[for each cross_ref:]
  [source_id: varu64][target_id: varu64]
  [ref_type: u8]
  [col_len: varu32][collection: UTF-8]
```

All length-prefixed fields use varint encoding for compact storage.

---

## 11. Checksums (CRC32)

### Algorithm

- **Standard**: CRC-32/ISO-HDLC (IEEE 802.3)
- **Polynomial**: `0xEDB88320` (reflected form of `0x04C11DB7`)
- **Initial value**: `0xFFFFFFFF`
- **Final XOR**: `0xFFFFFFFF`
- **Compatible with**: zlib, gzip, PNG CRC32

### Where Checksums Are Used

| Location | Stored At | Covers |
|:---------|:----------|:-------|
| Page checksum | Page header offset 28 (u32 LE) | Entire 4096-byte page (with checksum field zeroed) |
| WAL record checksum | Last 4 bytes of each record | All record bytes preceding the checksum |

Checksums are **verified on read** and **updated on write**. A mismatch on read indicates corruption.

---

## 12. Magic Bytes Reference

| Magic | Hex | Location | Purpose |
|:------|:----|:---------|:--------|
| `RDDB` | `52 44 44 42` | Page 0, offset 32 | Database header identity |
| `RDM2` | `52 44 4D 32` | Page 1, offset 32 | Metadata page (format V2) |
| `RDBW` | `52 44 42 57` | WAL file, offset 0 | Write-ahead log identity |
| `RDST` | `52 44 53 54` | Binary store file, offset 0 | Simple (non-paged) store format |

---

## 13. Physical Constants

| Constant | Value | Description |
|:---------|:------|:------------|
| `PAGE_SIZE` | 4096 bytes | Fixed for all pages |
| `HEADER_SIZE` | 32 bytes | Per-page header overhead |
| `CONTENT_SIZE` | 4064 bytes | Usable content per page |
| `MAX_CELLS` | ~676 | `(4064 - 4) / 6` |
| `FREE_IDS_PER_TRUNK` | 1014 | `(4096 - 32 - 8) / 4` |
| `MAX_KEY_SIZE` | 1024 bytes | B-tree key limit |
| `MAX_VALUE_SIZE` | 1024 bytes | B-tree inline value limit |
| `MIN_FILL_FACTOR` | 25% | B-tree merge threshold |
| `LEAF_DATA_OFFSET` | 40 | Leaf cell data start (`HEADER_SIZE + 8`) |
| `INTERIOR_DATA_OFFSET` | 32 | Interior cell data start (`HEADER_SIZE`) |

---

## 14. Corruption Defense Layers

RedDB implements 7 layers of corruption defense:

### Layer 1: Exclusive File Lock (`fs2`)

On `Pager::open()`, an exclusive advisory lock (`flock`) is acquired on the `.rdb` file. This prevents two processes from writing to the same database simultaneously. Read-only opens acquire a shared lock. Released automatically on drop.

### Layer 2: Double-Write Buffer (`.rdb-dwb`)

Before writing dirty pages to their final locations, they are first written to a separate `.rdb-dwb` file with a CRC32-verified header. If a crash occurs during the final write (torn page), the complete page can be recovered from the DWB on next open.

```
DWB format: [magic: "RDDW"][count: u32][checksum: u32][page_id: u32, page_data: 4096 bytes]...
```

### Layer 3: Header Shadow (`.rdb-hdr`)

Every time page 0 is written, a shadow copy is first written to `.rdb-hdr` and fsynced. If page 0 is corrupted on open, the shadow is used to recover it automatically.

### Layer 4: Metadata Shadow (`.rdb-meta`)

Same protection for page 1 (collection registry). A shadow copy is written to `.rdb-meta` before each `persist()`. If page 1 fails checksum on load, the shadow is used.

### Layer 5: Proper fsync

- `persist()` calls `pager.sync()` (flush + fsync), not just `flush()`
- `save_to_file()` calls `sync_all()` after writing
- All shadow and DWB files are fsynced before use

### Layer 6: Two-Phase Checkpoint

The checkpoint process is crash-safe via a two-phase protocol:

1. **PREPARE**: Set `checkpoint_in_progress=true` + `target_lsn` in header, fsync
2. **APPLY**: Write committed pages from WAL to database
3. **COMPLETE**: Set `checkpoint_in_progress=false` + update `checkpoint_lsn`, fsync
4. **TRUNCATE**: Truncate WAL

If a crash occurs between PREPARE and COMPLETE, recovery detects the flag and re-applies the WAL.

Header fields (page 0, offset 192+):

| Offset | Size | Field |
|:-------|:-----|:------|
| 192 | 1 | `checkpoint_in_progress` (0 or 1) |
| 193 | 8 | `checkpoint_target_lsn` (u64 LE) |

### Layer 7: Binary Store Integrity

Files saved via `save_to_file()` (V3+) use:
- **CRC32 footer**: 4-byte checksum appended after all data, verified on load
- **Atomic rename**: Written to `.rdb-tmp`, fsynced, then `rename()` to final path
- **Directory fsync**: Parent directory fsynced after rename for durability

### Companion Files

A RedDB database may have these companion files:

| File | Purpose |
|:-----|:--------|
| `.rdb` | Main database file |
| `.rdb-hdr` | Header shadow (page 0 backup) |
| `.rdb-meta` | Metadata shadow (page 1 backup) |
| `.rdb-dwb` | Double-write buffer (empty when idle) |
| `.wal` | Write-ahead log |

### Durability Summary

1. **WAL-first**: All changes written to WAL before modifying database pages
2. **CRC32 everywhere**: Pages, WAL records, binary store files, DWB
3. **No torn pages**: Double-write buffer catches partial writes
4. **No lost headers**: Shadow files for page 0 and page 1
5. **No corrupt checkpoints**: Two-phase protocol with recovery flag
6. **No concurrent corruption**: Exclusive file lock via flock
7. **Atomic binary saves**: Write-to-temp-then-rename pattern
8. **fsync discipline**: All critical writes followed by fsync
