# Events: SUPPRESS EVENTS in DML + replication from_replication gate [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/298

Labels: needs-triage

GitHub issue number: #298

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Bypass para 2 casos:
- Bulk INSERT explícito não quer disparar eventos.
- Replicação aplicando WAL no replica não duplica eventos do primary.

End-to-end:
- `INSERT INTO foo (..) VALUES (..) SUPPRESS EVENTS` — engine skipa subscriptions.
- `UPDATE foo SET .. WHERE .. SUPPRESS EVENTS` idem.
- Replication: contexto de mutation marca `from_replication: true`. Mutation pipeline gate skipa subscriptions.
- DDL: `SUPPRESS EVENTS` é per-statement, não persistente.

## Acceptance criteria

- [ ] `INSERT INTO foo VALUES (..) SUPPRESS EVENTS` não emite eventos.
- [ ] Replica aplicando WAL nunca duplica eventos do primary.
- [ ] Bulk INSERT 100k rows com SUPPRESS roda sem queue cheia.
- [ ] Conformance: 3 casos.

## Blocked by

- #292
