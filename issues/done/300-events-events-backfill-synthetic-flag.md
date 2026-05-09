# Events: EVENTS BACKFILL + synthetic flag [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/300

Labels: needs-triage

GitHub issue number: #300

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Comando `EVENTS BACKFILL` para enfileirar eventos sintéticos das rows existentes (replay/bootstrap downstream).

End-to-end:
- DDL: `EVENTS BACKFILL <collection> [WHERE <pred>] TO <queue> [LIMIT N]`.
- Lê snapshot da collection, batch enfileira eventos com `synthetic: true`.
- `event_id` determinístico: BLAKE3(`collection || id || "backfill" || subscription_id`). Re-run idempotente.
- Respeita REDACT da subscription target.
- Respeita tenant scope.
- Status em `EVENTS BACKFILL STATUS <collection>` (próxima slice expande em red.subscriptions).

## Acceptance criteria

- [ ] `EVENTS BACKFILL users TO audit` enfileira N eventos com `synthetic: true`.
- [ ] Re-run sem duplicar (idempotência via deterministic event_id).
- [ ] WHERE filter funcional.
- [ ] LIMIT N respeitado.
- [ ] REDACT da subscription aplicado em backfill events.
- [ ] Tenant scope respeitado.
- [ ] Conformance: 3 casos.

## Blocked by

- #292
