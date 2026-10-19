---
"@reddb-io/cli": patch
---

Preserve stored segment sequence identities when opening and replaying paged stores. Serialize automatic inserts after the segment assigns their identity, borrowing the final row instead of cloning its payload. Keep completed inline inserts reusable at checkpoints only when exactly that fenced mutation changed the manager; retain full comparison for untracked edits, publication changes and versioned rows. Seed reuse after durable WAL tree reconciliation to avoid immediately rebuilding recovered history. WAL acknowledgement, visibility checks and durable catalog publication remain required.
