---
"@reddb-io/cli": patch
---

Reconcile legacy and canonical system collection aliases when they share the same physical data and index roots but carry different resume counters, preserving the roots, recounting the shared tree, and retaining conservative identity high-water marks.
