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
