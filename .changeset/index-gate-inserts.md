---
"@reddb-io/cli": patch
---

Concurrent INSERTs no longer turn indexed reads into table scans. A reader gave up on every index whenever any statement was in flight or had started since its snapshot, including autocommit inserts on other tables: with four connections inserting, a lookup by id on a 45k-row table went from 2 ms to 650 ms. An insert adds index entries and removes none, so it now only covers the window between publishing its row and its index entries, on its own collection, and a reader waits a few milliseconds for the inserts it can see to finish instead of scanning. Updates, deletes, explicit transactions and replayed changes keep the previous rule.
