---
"@reddb-io/cli": patch
---

After a restart that replays the WAL, a range read through a BTREE index could miss rows written since the last checkpoint: the index started with no record of them and answered from the published tree alone. It now starts from what recovery replayed, as the HASH index already did. Both indexes also relearn the rows removed since the last checkpoint, so a lookup whose published postings name one of them no longer walks the whole collection.
