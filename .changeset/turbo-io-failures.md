---
"reddb-io-server": patch
---

Return vector.turbo WAL and extent I/O failures instead of acknowledging them, while preserving
the authoritative vector entity for immediate access and cold rebuild.
