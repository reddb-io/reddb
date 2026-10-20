---
"@reddb-io/cli": patch
---

Build each catalog model snapshot from one physical metadata observation instead of reloading it independently for indexes, graph projections and analytics jobs. Preserve index reconciliation and fallback behavior while reducing repeated work and publication-lock waits during checkpoints.
