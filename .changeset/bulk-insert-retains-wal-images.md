---
"@reddb-io/cli": patch
---

Fix checkpoints rewriting a whole collection after a batch insert on paged stores without bounded residency: once the base is immutable the batch skips the published tree, and its WAL images were not retained for the incremental checkpoint.
