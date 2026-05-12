# Catalog: red.policies virtual table + SHOW POLICIES ON <name> [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/256

Labels: enhancement

GitHub issue number: #256

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Virtual table `red.policies` + comando `SHOW POLICIES ON <name>` que lista IAM policies + RLS predicates anexados a uma collection.

End-to-end:
- `red.policies` schema: `name`, `collection`, `kind` (iam | rls), `effect` (allow | deny), `actions` (array), `principals` (array), `predicate` (string for RLS), `enabled`.
- Materializa de auth registry + RLS policy registry.
- `SHOW POLICIES [ON <name>]` desugar para `SELECT * FROM red.policies [WHERE collection = '<name>']`.

## Acceptance criteria

- [ ] `SHOW POLICIES ON users` lista policies anexadas (IAM + RLS).
- [ ] Sem `ON`, lista todas policies do tenant.
- [ ] RLS predicate aparece como string raw (não AST estruturado).
- [ ] Tenant filter aplicado.
- [ ] Conformance corpus: 2 casos.
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
