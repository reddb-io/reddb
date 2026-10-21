---
"@reddb-io/cli": patch
---

Fence row mutations and delete stamps through their WAL append against checkpoint
publication. Transactions starting during a checkpoint cannot leak uncommitted
rows, and concurrent acknowledged autocommit updates retain their recovery WAL.
