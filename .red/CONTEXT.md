# RedDB Domain Glossary

> **This glossary has been sharded.** The canonical entry point is now **[CONTEXT-MAP.md](CONTEXT-MAP.md)**, with topic children under [`context/`](context/).

Reusable vocabulary for code, docs, and architecture decisions. New terms join the relevant child file as they crystallize during design discussions.

- **[CONTEXT-MAP.md](CONTEXT-MAP.md)** — index + `Storage/deploy profile` + `Performance gate`
- **[context/persistence.md](context/persistence.md)** — storage engine shared by all profiles (cache, WAL, manifest, file layout, DDL, backup/restore)
- **[context/standalone.md](context/standalone.md)** — embedded single-file profile + migration path
- **[context/serverless.md](context/serverless.md)** — serverless snapshot/segment-pack profile
- **[context/primary-replica.md](context/primary-replica.md)** — primary + replicas (read routing, freshness, promotion, shared replication mechanics)
- **[context/clustering.md](context/clustering.md)** — multi-writer cluster (ownership, supervisor, fencing, rebalancing, cross-range)
- **[context/data-model.md](context/data-model.md)** — query/streams, catalog, keyed models, events, queues, analytics
- **[context/governance.md](context/governance.md)** — auth & security, telemetry, compliance
