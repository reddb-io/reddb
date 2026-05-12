# Column policy: ColumnPolicyGate deep module [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/264

Labels: enhancement

GitHub issue number: #264

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#240

## What to build

Módulo central que aplica column-level deny gating. Reusado por todos os query paths em slices subsequentes.

End-to-end:
- Novo módulo `auth/column_policy_gate.rs` com interface única:
  - `gate(principal, action, columns: &[QualifiedColumn]) -> GateResult`
  - `GateResult`: `Allowed`, `DeniedColumns(Vec<QualifiedColumn>)`, `Error`
- Reusa o IAM policy matching engine existente (`auth_ddl.rs` patterns).
- Suporta wildcards: `column:*.email`, `column:users.*`.
- Suporta JSON path normalization (decisão da slice 18, mas o gate aceita já): `body.email` é uma `QualifiedColumn` válida.
- Cache de decisões por (principal, action, table) por session.
- Audit trail: cada gate decision logged via `OperatorEvent`.

## Acceptance criteria

- [ ] `gate(alice, Select, ["users.email"])` retorna `DeniedColumns(["users.email"])` se policy nega.
- [ ] Wildcard `column:*.email` matches em qualquer collection.
- [ ] Cache funciona: gate chamado 10x retorna mesmo result em sub-µs.
- [ ] Audit log captura cada decisão com principal + action + denied columns.
- [ ] Module 100% testado isolado (sem dependência de query paths).
- [ ] Performance: gate < 10µs em hot path.

## Blocked by

- #247
