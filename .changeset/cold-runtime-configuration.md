---
"@reddb-io/cli": patch
---

Resolve typed runtime configuration and persisted ranking configuration through
the shared disk-and-resident configuration reader. Preserve operator settings
when checkpointed configuration leaves RAM, and prevent bootstrap defaults or
mounted overlays from replacing persisted keys. Keep persisted queue consumer
groups readable after the same checkpoint.
