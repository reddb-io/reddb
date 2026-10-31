---
"@reddb-io/cli": patch
---

Fix concurrent page-cache slot publication so repeated inserts cannot retain unindexed page images beyond cache capacity. Revalidate slot identity for reads, pins and dirty-state updates, and recheck pins and dirty images when an eviction takes ownership of its writeback page.
