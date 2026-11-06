---
"@reddb-io/cli": patch
---

Reconcile populated legacy and canonical system collection names only when their complete physical state is identical, preserving shared data and index roots while continuing to fail closed on divergent collections.
