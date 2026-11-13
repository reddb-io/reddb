---
"@reddb-io/cli": patch
---

Preserve the local authoritative database when a primary/replica or cluster node acquires a writer lease. A backend used for ownership fencing no longer activates serverless snapshot hydration or replaces a promoted replica with an older remote snapshot. Embedded and serverless snapshot publication keep their existing behavior.

Keep replica checkpoints local, and upload native writer checkpoints through a consistent copy made with the pager's existing file handle. This preserves manual checkpoint backups without reopening a live locked database file on Windows.
