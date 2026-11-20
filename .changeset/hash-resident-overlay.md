---
"@reddb-io/cli": patch
---

Paged HASH index lookups no longer walk every resident row of the collection: resident changes are tracked in an overlay, like the paged BTREE index.
