---
"@reddb-io/cli": patch
---

Preserve all configuration records when offline compaction retires the obsolete physical `red_config` collection: promote the complete populated tree into an empty `red.config` identity, report the migrated records, and fail closed when both identities contain data.
