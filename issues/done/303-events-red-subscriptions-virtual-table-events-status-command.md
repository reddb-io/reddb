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

## Progress note - 2026-05-09

Implemented the separable #303 slice that does not require #300:

- Added runtime virtual table `red.subscriptions` with columns `name`, `collection`, `target_queue`, `mode`, `ops_filter`, `where_filter`, `redact_fields`, `enabled`, `outbox_lag_ms`, `dlq_count`, `created_at`.
- Added parser desugar for `EVENTS STATUS [<collection>]` to `SELECT * FROM red.subscriptions [WHERE collection = '<name>']`.
- Added parser conformance cases for `EVENTS STATUS`, `EVENTS STATUS users`, and `EVENTS STATUS users ORDER BY target_queue`.
- Added runtime coverage for `SELECT ... FROM red.subscriptions`, `EVENTS STATUS users`, and outbox DLQ count reporting.
- Updated `docs/query/events.md` and `docs/reference/red-schema.md`.

Remaining blocked work after integration:

- #300 now provides `EVENTS BACKFILL`, synthetic events, and deterministic
  idempotency, but there is still no durable backfill-progress runtime/source of
  truth to query.
- `EVENTS BACKFILL STATUS <collection>` therefore still returns an explicit
  not-implemented error instead of rows processed / ETA.
