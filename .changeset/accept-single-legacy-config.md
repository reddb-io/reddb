---
"@reddb-io/cli": patch
---

Allow canonical configuration recovery to migrate historical databases that contain only the populated `red_config` identity, preserving its complete tree as `red.config`; treat canonical-only databases as already migrated and continue to fail closed on missing or ambiguous identities.
