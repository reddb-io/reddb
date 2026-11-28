---
"@reddb-io/cli": patch
---

Version reclaim and retention no longer make a checkpoint copy a large collection. A checkpoint writes a collection's changes as a delta only up to 8,192 rows; reclaim took 512 rows a second, so over a large backlog every checkpoint copied the collection again with all writers waiting (two minutes for a 580,000-row ledger). On a collection above 32,768 rows, maintenance now takes 2,048 rows of each checkpoint's delta and continues after the checkpoint.

`CREATE UNIQUE INDEX` checks the rows on disk for duplicates. It read only the rows in memory, so a table whose duplicates had been checkpointed got its unique index.

A slow checkpoint also logs where its publication step spent the time (`checkpoint publication`).
