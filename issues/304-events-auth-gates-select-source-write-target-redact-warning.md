# Events: auth gates — select source + write target + REDACT warning [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/304

Labels: needs-triage

GitHub issue number: #304

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Validação de DDL: subscription só pode ser criada se principal tem `select` source + `write` target. Warning quando source tem column policies não cobertas por REDACT.

End-to-end:
- `CREATE TABLE users WITH EVENTS TO audit` → engine valida: principal tem `select` em users? `write` em audit? Se não → 403.
- `ALTER TABLE users ADD SUBSCRIPTION ... TO audit` → mesma validação.
- Warning na DDL: source `users` tem `DENY select ON column:users.email`, mas REDACT não cobre `email` → log warning + audit event "subscription_redact_gap" (não bloqueia DDL).
- Capability `events:cluster_subscribe` para cross-tenant (já tratado em #293, mencionar aqui).

## Acceptance criteria

- [ ] DDL sem `select` em source → 403.
- [ ] DDL sem `write` em target → 403.
- [ ] Source com column policy + REDACT cobrindo → DDL OK sem warning.
- [ ] Source com column policy + REDACT incompleto → DDL OK com warning (não 403).
- [ ] Conformance: 4 casos.

## Blocked by

- #294
- #295
