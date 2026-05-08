# `red.*` Schema Reference

RedDB exposes internal metadata through read-only `red.*` collections. These
collections are queryable with ordinary SQL once the corresponding runtime
catalog is available.

## `red.collections`

`SHOW COLLECTIONS` is syntax sugar for:

```sql
SELECT * FROM red.collections;
```

Filters are preserved during desugaring:

```sql
SHOW COLLECTIONS WHERE model = 'table';
```

Current columns:

| Column            | Description |
|-------------------|-------------|
| `name`            | Collection name. |
| `model`           | Logical model, such as table, document, graph, vector, queue, time-series, or mixed. |
| `schema_mode`     | Schema contract mode for the collection. |
| `entities`        | Number of live entities in the collection. |
| `segments`        | Number of backing storage segments. |
| `indices`         | Number of secondary indexes attached to the collection. |
| `in_memory_bytes` | Approximate resident memory used by collection metadata and caches. |
| `tenant_id`       | Tenant owning the collection, or `NULL` for global/unscoped collections. |
