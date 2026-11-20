---
"@reddb-io/cli": patch
---

Paged HASH index lookups no longer walk the collection before the first checkpoint: while a collection has no disk base, the resident overlay holds every row and answers alone. This also keeps batch loads into tables with a PRIMARY KEY or UNIQUE constraint linear.
