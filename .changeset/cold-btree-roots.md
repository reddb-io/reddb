---
"@reddb-io/cli": patch
---

Publish disk-resident BTREE and equality roots with their index descriptor so cold startup reuses the compacted trees instead of rebuilding them and regrowing storage.
