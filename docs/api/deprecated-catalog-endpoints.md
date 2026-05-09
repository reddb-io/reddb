# Deprecated Catalog Endpoints

The granular `GET /catalog/*` catalog endpoints below are deprecated and sunset on 2026-08-08. Responses include:

- `Deprecation: 2026-08-08`
- `Sunset: 2026-08-08`

Use `POST /query` with the corresponding `red.*` SQL relation instead. `GET /catalog`, `GET /catalog/readiness`, `GET /catalog/attention`, and `GET /catalog/consistency` are not deprecated.

| Legacy endpoint | SQL substitute |
|:----------------|:---------------|
| `GET /catalog/indexes/declared` | `SELECT * FROM red.indices WHERE declared = true` |
| `GET /catalog/indexes/operational` | `SELECT * FROM red.indices WHERE operational = true` |
| `GET /catalog/indexes/status` | `SELECT * FROM red.indices` |
| `GET /catalog/indexes/attention` | `SELECT * FROM red.indices WHERE requires_rebuild = true OR in_sync = false OR queryable = false` |
| `GET /catalog/graph/projections/declared` | `SELECT * FROM red.graph_projections WHERE declared = true` |
| `GET /catalog/graph/projections/operational` | `SELECT * FROM red.graph_projections WHERE operational = true` |
| `GET /catalog/graph/projections/status` | `SELECT * FROM red.graph_projections` |
| `GET /catalog/graph/projections/attention` | `SELECT * FROM red.graph_projections WHERE needs_attention = true` |
| `GET /catalog/analytics-jobs/declared` | `SELECT * FROM red.analytics_jobs WHERE declared = true` |
| `GET /catalog/analytics-jobs/operational` | `SELECT * FROM red.analytics_jobs WHERE operational = true` |
| `GET /catalog/analytics-jobs/status` | `SELECT * FROM red.analytics_jobs` |
| `GET /catalog/analytics-jobs/attention` | `SELECT * FROM red.analytics_jobs WHERE needs_attention = true` |
| `GET /catalog/collections/readiness` | No exact implemented `red.*` equivalent yet; use `SELECT name, model, schema_mode, entities, segments, internal, tenant_id FROM red.collections` plus `red.stats` for operational counters. |
| `GET /catalog/collections/readiness/attention` | No exact implemented `red.*` equivalent yet; use `SELECT * FROM red.stats WHERE attention_score > 0` for currently exposed collection attention. |
