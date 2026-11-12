---
"@reddb-io/cli": patch
---

Preserve the local authoritative database when a primary/replica or cluster node acquires a writer lease. A backend used for ownership fencing no longer activates serverless snapshot hydration or replaces a promoted replica with an older remote snapshot. Embedded and serverless snapshot publication keep their existing behavior.
