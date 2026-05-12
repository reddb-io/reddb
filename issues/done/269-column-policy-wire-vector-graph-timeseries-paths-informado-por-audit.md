# Column policy: wire vector/graph/timeseries paths (informado por audit) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/269

Labels: enhancement

GitHub issue number: #269

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#240

## What to build

Wire column gate nos paths não-relational, conforme priorização do audit pass (#247).

Paths esperados (sujeito a confirmation pelo audit):
- `runtime/query_exec/vector.rs` — vector search com `RETURNING <fields>`
- `runtime/query_exec/graph.rs` — graph traversal com property projection
- `runtime/query_exec/timeseries.rs` — `SELECT metric, value, tags`

End-to-end:
- Para cada path, identificar onde a column projection é resolvida.
- Inserir chamada a `ColumnPolicyGate::gate`.
- Coluna negada: null ou erro (igual SELECT relacional).
- Conformance + integration tests por path × model.

## Acceptance criteria

- [ ] Vector search com policy column-deny respeita gate.
- [ ] Graph traversal idem.
- [ ] Timeseries idem.
- [ ] Conformance corpus: ≥3 casos (1 por path).
- [ ] Audit doc atualizado refletindo paths agora cobertos.

## Blocked by

- #247
- #264
