---
"@reddb-io/client": patch
---

Bound collection-scan payload allocation to the requested page. Traverse physical
entities in stable ID order without cloning the whole collection, preserve owned
row schemas, and pass HTTP offsets directly as runtime cursors instead of
materializing the skipped prefix. Retain total counts, next cursors and empty
beyond-end pages, including maximum-size offsets. Add allocation regressions over
the runtime and real HTTP transport.
