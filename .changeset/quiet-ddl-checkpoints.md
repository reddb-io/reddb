---
"@reddb-io/cli": patch
---

Keep metadata-only DDL publication bounded by avoiding an unrelated full payload checkpoint after cold WAL replay.
