# Page-Based Storage

RedDB uses a page-based storage engine for durable persistence. All data is organized into fixed-size pages written to a single database file.

## Page Layout

Each page has a header and a data region:

```
+-------------------+
| Page Header (16B) |
|   - page_id       |
|   - page_type     |
|   - checksum      |
|   - next_page     |
+-------------------+
| Page Data          |
|   (variable)       |
+-------------------+
```

## Page Types

| Type | Purpose |
|:-----|:--------|
| Header | Database metadata and configuration |
| BTree Internal | B-Tree internal nodes (keys + child pointers) |
| BTree Leaf | B-Tree leaf nodes (keys + values) |
| Overflow | Large values spanning multiple pages |
| Free | Recycled page in the free list |
| Collection Root | Root page for a collection's B-Tree |

## File Organization

```
+-----+-----+-----+-----+-----+-----+-----+
| HDR | CR1 | CR2 | BT  | BT  | OVF | FRE |
+-----+-----+-----+-----+-----+-----+-----+
  ^     ^     ^     ^     ^     ^     ^
  |     |     |     |     |     |     Free list
  |     |     |     |     |     Overflow page
  |     |     |     B-Tree pages
  |     |     Collection root (coll 2)
  |     Collection root (coll 1)
  Database header
```

## Memory-Mapped I/O

For read-heavy workloads, the pager can use memory-mapped files:

- Pages are mapped directly into the process address space
- Read operations avoid system call overhead
- The OS page cache handles eviction

## In-Memory Mode

When no `--path` is specified, the pager operates entirely in RAM:

- Zero disk I/O
- Maximum performance for development and testing
- Data is lost when the process exits

## Paged Mode vs Direct Mode

| Feature | Paged (file-backed) | Direct (in-memory) |
|:--------|:-------------------|:-------------------|
| Persistence | Yes | No |
| WAL | Yes | No |
| Crash recovery | Yes | No |
| Memory limit | Disk-based | RAM-based |
| Latency | Microseconds | Nanoseconds |

---

## Corruption Defense

The pager implements multiple layers of protection against data corruption. See [File Format: Corruption Defense](/engine/file-format.md#14-corruption-defense-layers) for the full specification.

### File Lock

On `Pager::open()`, an exclusive advisory lock (`flock` via `fs2`) is acquired on the `.rdb` file. This prevents two processes from writing to the same database simultaneously. Read-only opens acquire a shared lock. Released automatically when the Pager is dropped.

### Double-Write Buffer (`.rdb-dwb`)

Before writing dirty pages to their final locations, all pages in a flush batch are first written to a `.rdb-dwb` companion file with a CRC32-verified header:

```
[magic: "RDDW"][count: u32][checksum: u32][page_id + page_data]...
```

1. Write all dirty pages to DWB, fsync
2. Write pages to final locations in `.rdb`
3. Truncate DWB (marks as consumed)

If a crash occurs during step 2 (torn page), the next `Pager::open()` detects the non-empty DWB and re-applies the intact pages.

### Header & Metadata Shadows

| Shadow file | Protects | Updated when |
|:------------|:---------|:-------------|
| `.rdb-hdr` | Page 0 (database header) | Every `write_header()` |
| `.rdb-meta` | Page 1 (collection registry) | Every `persist()` |

Shadows are written and fsynced **before** the main page is written. If the main page is corrupted on open, the pager automatically recovers from the shadow.

### Companion Files

A RedDB database produces these files:

| File | Purpose | Size |
|:-----|:--------|:-----|
| `data.rdb` | Main database | Variable |
| `data.rdb-hdr` | Header shadow | 4 KB |
| `data.rdb-meta` | Metadata shadow | 4 KB |
| `data.rdb-dwb` | Double-write buffer | Empty when idle |
| `data.rdb-wal` | Write-ahead log | Variable |
