---
"@reddb-io/cli": patch
---

Keep captured transaction inserts and row upserts out of published B-tree pages until their committed WAL is durable. This prevents incidental pager flushes from recovering unfinished inserts or delete stamps after a crash. Add crash regressions for INSERT, bulk INSERT, UPDATE and DELETE with open, rolled-back and committed transactions.
