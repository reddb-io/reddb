---
"@reddb-io/cli": patch
---

Reject cyclic overflow page chains with an explicit corruption error so reads and recovery cannot spin forever.
