---
"@reddb-io/cli": patch
---

Avoid rewriting an unchanged collection during paged checkpoints when a pending
transaction and updates to more than 1,024 rows exhaust retained row images.
Keep bounded references to the existing durable WAL, compare exact row bytes,
invalidate proofs on WAL truncation, and project updates in bounded batches.
Persistent file and WAL formats remain unchanged.
