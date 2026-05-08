# Deprecated Catalog Endpoints

The granular `GET /catalog/*` catalog endpoints below are deprecated and sunset on 2026-08-08. Responses include:

- `Deprecation: 2026-08-08`
- `Sunset: 2026-08-08`

Use `POST /query` with the corresponding `red.*` SQL relation instead. `GET /catalog`, `GET /catalog/readiness`, `GET /catalog/attention`, and `GET /catalog/consistency` are not deprecated.

| Legacy endpoint | SQL substitute |
|:----------------|:---------------|
| `GET /catalog/indexes/declared` | `SELECT * FROM red.indexes WHERE declared = true` |
| `GET /catalog/indexes/operational` | `SELECT * FROM red.indexes WHERE operational = true` |
| `GET /catalog/indexes/status` | `SELECT * FROM red.indexes` |
| `GET /catalog/indexes/attention` | `SELECT * FROM red.indexes WHERE needs_attention = true` |
| `GET /catalog/graph/projections/declared` | `SELECT * FROM red.graph_projections WHERE declared = true` |
| `GET /catalog/graph/projections/operational` | `SELECT * FROM red.graph_projections WHERE operational = true` |
| `GET /catalog/graph/projections/status` | `SELECT * FROM red.graph_projections` |
| `GET /catalog/graph/projections/attention` | `SELECT * FROM red.graph_projections WHERE needs_attention = true` |
| `GET /catalog/analytics-jobs/declared` | `SELECT * FROM red.analytics_jobs WHERE declared = true` |
| `GET /catalog/analytics-jobs/operational` | `SELECT * FROM red.analytics_jobs WHERE operational = true` |
| `GET /catalog/analytics-jobs/status` | `SELECT * FROM red.analytics_jobs` |
| `GET /catalog/analytics-jobs/attention` | `SELECT * FROM red.analytics_jobs WHERE needs_attention = true` |
| `GET /catalog/collections/readiness` | `SELECT name, query_ready, write_ready, repair_ready FROM red.collections` |
| `GET /catalog/collections/readiness/attention` | `SELECT name, query_ready, write_ready, repair_ready FROM red.collections WHERE query_ready = false OR write_ready = false OR repair_ready = false` |
