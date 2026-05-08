# Parser conformance: full positive corpus (~40 forms de docs + landing) [AFK]

## Parent

#227

## What to build

Expandir o conformance corpus com TODA forma de SQL/RQL atualmente documentada. Cada caso aponta para `source` (arquivo + linha) onde aparece.

Fontes a varrer:
- `reddb/README.md`
- `reddb/docs/data-models/*.md` (queues, documents, timeseries, kv, vectors, graph)
- `reddb/docs/query/*.md` (select, insert, update, delete, joins, transactions)
- `reddb/docs/guides/*.md` (vector clustering, eventual consistency, ask-your-database, etc)
- `rdb-lair/apps/landing/src/routes/+page.svelte`
- `rdb-lair/apps/landing/src/lib/data/data-types.ts`

Estimativa: ~40 cases positivos novos (alguns podem ser variantes do mesmo form, mantém todos).

## Acceptance criteria

- [ ] ≥40 casos positivos em `tests/conformance/positive/`.
- [ ] Cada caso tem `source` válido (arquivo + linha que existe hoje).
- [ ] Categorias cobertas: SELECT (incl. joins, group by, window), INSERT (table/document/timeseries/kv/vector/edge/node), CREATE (TABLE/INDEX/QUEUE/TIMESERIES/VIEW), UPDATE, DELETE, QUEUE commands, GRAPH MATCH/PATH, VECTOR SEARCH, HYBRID, FROM ANY.
- [ ] `cargo test -p reddb-server --test conformance` passa em todos os 40.
- [ ] Script auxiliar (Python ou shell) que valida que cada `source` aponta para arquivo+linha existente — falha se um exemplo é movido.

## Blocked by

- #229
