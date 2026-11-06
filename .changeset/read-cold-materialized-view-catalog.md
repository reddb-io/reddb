---
"@reddb-io/cli": patch
---

Read materialized-view definitions from disk-backed collections during boot, replacement and deletion. Decode both named and schema-backed rows, propagate storage scan failures, and prevent descriptor accumulation across cold restarts.
