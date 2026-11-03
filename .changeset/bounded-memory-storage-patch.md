---
"@reddb-io/sdk": patch
---

Fix memory ownership through query results, binary encoders and RedWire sends; admit index candidates before allocation and release completed transport buffers.

Fix checkpoint progress and cold MVCC reclamation so committed data can outgrow the resident arena, active transactions remain visible, and paused cleanup does not retain page generations that force full-history copies. Preserve cold catalog models, counts and audit visibility.

Add explicit opt-in disk residency for standalone local single-file stores, generation-bound native HASH/BTREE projections, and an offline compactor that verifies persisted records while preserving its source. Preparing disk-resident files fences older writers; retain an independent unconverted backup for rollback. Replicated, remote and serverless profiles remain rejected by the explicit disk-residency option.

Keep the feature disabled by default. Historical-copy and workload measurements describe the tested scenarios and do not establish a global process-memory ceiling. CI now reports the first test failure without automatic retries and isolates client release-size artifacts from test runners.
