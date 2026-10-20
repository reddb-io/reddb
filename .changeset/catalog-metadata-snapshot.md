---
"@reddb-io/cli": patch
---

Build each catalog model snapshot from one physical metadata observation instead of reloading it independently for indexes, graph projections and analytics jobs. Serverless readiness checks evaluate native completeness directly without computing unrelated metadata-repair diagnostics. Preserve index reconciliation, readiness criteria and fallback behavior while reducing repeated work and publication-lock waits during checkpoints.
