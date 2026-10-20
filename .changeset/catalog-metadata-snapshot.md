---
"@reddb-io/cli": patch
---

Build each catalog model snapshot from one physical metadata observation instead of reloading it independently for indexes, graph projections and analytics jobs. Serverless readiness checks evaluate native completeness directly without computing unrelated metadata-repair diagnostics. Compute aggregate readiness from one health observation and reuse that observation in catalog HTTP responses, instead of recomputing the complete health report for each scalar flag. Preserve index reconciliation, readiness criteria and fallback behavior while reducing repeated work and publication-lock waits during checkpoints.
