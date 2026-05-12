# Events: REDACT clause (strip fields at producer) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/294

Labels: enhancement

GitHub issue number: #294

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Aplica REDACT no payload antes de enfileirar. Subscription com `REDACT (email, phone)` produz eventos sem esses campos.

End-to-end:
- Payload builder consulta `subscription.redact_fields` antes de serialização.
- Strip de fields em `before` e `after`.
- REDACT funciona em flat e nested fields (`body.user.email`).
- Wildcard support: `REDACT (body.*.email)` strip qualquer email em sub-objects.

## Acceptance criteria

- [ ] `WITH EVENTS REDACT (email)` em users: payload nunca contém `email`.
- [ ] Wildcard nested: `REDACT (body.*.email)` strip aninhado.
- [ ] Multiple REDACT fields: `REDACT (email, phone, ssn)` strip todos.
- [ ] REDACT em DELETE event: `before` strippado também.
- [ ] Conformance: 4 casos (flat field, nested, wildcard, DELETE).

## Blocked by

- #292
