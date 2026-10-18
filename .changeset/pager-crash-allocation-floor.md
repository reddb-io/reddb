---
"@reddb-io/cli": patch
---

Recover the pager allocation floor from the physical file extent before allocating after a crash. Pages written by cache eviction or double-write recovery can reach disk before the allocation counter in the superblock; reopening could otherwise allocate over those existing pages. Partial trailing pages are reserved without being trusted, and read-only opens do not change the file.
