# Events: red.subscriptions virtual table + EVENTS STATUS command [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/303

Labels: needs-triage

GitHub issue number: #303

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Introspection layer: virtual table `red.subscriptions` + comando `EVENTS STATUS`.

End-to-end:
- `red.subscriptions` schema: `name`, `collection`, `target_queue`, `mode` (FANOUT|WORK), `ops_filter`, `where_filter`, `redact_fields`, `enabled`, `outbox_lag_ms`, `dlq_count`, `created_at`.
- `EVENTS STATUS [<collection>]` desugar para `SELECT * FROM red.subscriptions [WHERE collection = '<name>']`.
- `EVENTS BACKFILL STATUS <collection>` mostra progresso (linhas processadas, ETA).
- Doc atualizado em `docs/reference/red-schema.md`.

## Acceptance criteria

- [ ] `SELECT * FROM red.subscriptions` lista subscriptions visíveis ao tenant.
- [ ] `EVENTS STATUS users` mostra subscriptions de `users` + lag + DLQ count.
- [ ] `EVENTS BACKFILL STATUS users` mostra progresso quando ativo.
- [ ] Conformance: 3 casos.
- [ ] Doc atualizado.

## Blocked by

- #292
- #300
