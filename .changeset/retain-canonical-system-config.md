---
"@reddb-io/cli": patch
---

Add an explicit offline compaction repair that retires a divergent legacy `red_config` collection while preserving `red.config` as the canonical system configuration store, leaving the source unchanged and failing closed on ambiguous catalog shapes or cross-references.
