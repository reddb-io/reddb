---
"@reddb-io/cli": patch
---

Preserve correct BTREE range reads after a crash during checkpoint publication when WAL recovery retires a row still referenced by the previous index generation.
