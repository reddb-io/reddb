---
"@reddb-io/cli": patch
---

Skip republishing an embedded `.rdb` snapshot that is byte-identical to the durable one over an empty WAL, so closing a store no longer rewrites the whole image two or three times.
