---
"@reddb-io/cli": patch
---

Restore shared column names on owned reads of reopened positional rows. Preserve
untouched values during SQL updates and include positional rows when rebuilding
secondary indexes without changing the file format or copying payloads again.
