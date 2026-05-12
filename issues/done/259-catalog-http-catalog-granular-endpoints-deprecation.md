# Catalog: HTTP /catalog/* granular endpoints deprecation [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/259

Labels: enhancement

GitHub issue number: #259

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Marca endpoints granulares de `/catalog/*` como deprecated. Header + log warning + tabela em docs apontando substituição via `red.*`.

Endpoints afetados (deprecated):
- `/catalog/indexes/declared`
- `/catalog/indexes/operational`
- `/catalog/indexes/status`
- `/catalog/indexes/attention`
- `/catalog/graph/projections/declared`
- `/catalog/graph/projections/operational`
- `/catalog/graph/projections/status`
- `/catalog/graph/projections/attention`
- `/catalog/analytics-jobs/declared`
- `/catalog/analytics-jobs/operational`
- `/catalog/analytics-jobs/status`
- `/catalog/analytics-jobs/attention`
- `/catalog/collections/readiness`
- `/catalog/collections/readiness/attention`

Mantidos (não deprecated):
- `GET /catalog`
- `GET /catalog/readiness`
- `GET /catalog/attention`
- `GET /catalog/consistency`

End-to-end:
- Cada response dos deprecated tem header `Deprecation: 2026-08-08` (3 meses) + `Sunset: 2026-08-08`.
- Log warning estruturado por chamada (rate-limited 1/min/endpoint).
- Doc nova: `docs/api/deprecated-catalog-endpoints.md` com tabela: endpoint legacy → query SQL `SELECT FROM red.X` substituta.
- `docs/api/http.md` marca endpoints com `**DEPRECATED**`.

## Acceptance criteria

- [ ] Cada endpoint deprecated retorna header `Deprecation` + `Sunset`.
- [ ] Log warning emitido (rate-limited).
- [ ] `docs/api/deprecated-catalog-endpoints.md` criado com tabela substitutiva.
- [ ] `docs/api/http.md` atualizado com flag DEPRECATED.
- [ ] Endpoints continuam funcionando (não removidos nesta slice).
- [ ] Test: GET retorna 200 + header `Deprecation` presente.

## Blocked by

- #244
