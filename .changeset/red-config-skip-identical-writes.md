---
"@reddb-io/cli": patch
---

Skip config writes that would store the value a key already holds, so opening an embedded store no longer issues dozens of durable WAL appends and fsyncs to re-seed unchanged config.
