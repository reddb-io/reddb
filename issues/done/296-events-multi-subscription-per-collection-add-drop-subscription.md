# Events: multi-subscription per collection (ADD/DROP SUBSCRIPTION) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/296

Labels: needs-triage

GitHub issue number: #296

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Permite N subscriptions por collection, cada uma com sua queue + redaction + filtros próprios.

End-to-end:
- DDL: `ALTER TABLE users ADD SUBSCRIPTION audit_minimal TO compliance_audit REDACT (email, phone)`.
- DDL: `ALTER TABLE users DROP SUBSCRIPTION audit_minimal`.
- `CollectionDescriptor.subscriptions` é Vec — múltiplos entries.
- Cada mutation: engine itera all subscriptions, para cada uma gera payload + push (respeitando ops_filter, where_filter, redact).

## Acceptance criteria

- [ ] `ALTER TABLE users ADD SUBSCRIPTION s1 TO q1` + `ADD SUBSCRIPTION s2 TO q2` cria 2 subscriptions.
- [ ] INSERT em users → 1 evento em q1 + 1 evento em q2.
- [ ] s1 com REDACT (email) e s2 sem: q1 sem email, q2 com email.
- [ ] DROP SUBSCRIPTION s1 → INSERT só vai pra q2.
- [ ] Conformance: 3 casos.

## Blocked by

- #292
