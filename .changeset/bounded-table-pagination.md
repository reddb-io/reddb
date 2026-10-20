---
"@reddb-io/cli": patch
---

Bound materialization for simple table LIMIT/OFFSET windows and avoid reserving
memory for skipped rows or arbitrarily large requested limits.
