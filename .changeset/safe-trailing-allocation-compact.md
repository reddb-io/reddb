---
"@reddb-io/cli": patch
---

Allow offline compaction to discard page-aligned physical allocation beyond the
committed database high-water mark while still rejecting truncated or partial
pages.
