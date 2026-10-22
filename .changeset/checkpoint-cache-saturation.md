---
"@reddb-io/cli": patch
---

Keep accepting bounded checkpoint WAL images after cache saturation. Unrelated row or byte pressure no longer disables incremental checkpoints for later writes; evicted or oversized images retain conservative validation and existing publication fences.
