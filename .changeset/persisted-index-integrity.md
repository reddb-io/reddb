---
"@reddb-io/cli": minor
---

Add an explicit runtime integrity check for persisted HASH and BTREE indexes, comparing checkpoint postings with authoritative rows to detect missing entries even when page checksums remain valid. Qualify checksum-damaged index recovery and logical index rebuilds across MVCC updates, deletes, checkpoints and cold reopens.
