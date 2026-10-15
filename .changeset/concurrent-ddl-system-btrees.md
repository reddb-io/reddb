---
"@reddb-io/cli": patch
---

Concurrent DDL no longer corrupts the internal B-trees (`red.control_events`, `red_index_registry`, `red_stats`). Several processes running the same idempotent migration at once could leave every later `CREATE TABLE` / `ALTER TABLE` failing, or wedge the server at capacity. DDL statements now run one at a time; a DDL that cannot get the catalog lock within `concurrency.locking.deadlock_timeout_ms` fails with a retryable error instead of running unlocked. Single-row writes into one table no longer race each other inside its B-tree, and the physical metadata file is replaced atomically.
