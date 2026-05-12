# Events: foundation — WITH EVENTS parser + subscription registry + cycle detector [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/291

Labels: enhancement

GitHub issue number: #291

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Foundation slice. Estabelece DDL + persistence + cycle detection. Ainda não emite eventos (slice 2 faz).

End-to-end:
- Parser: `CREATE TABLE foo WITH EVENTS [(<ops>)] [TO <queue>] [REDACT (<fields>)] [WHERE <pred>]` + ALTER variants.
- AST: `SubscriptionDescriptor` com source, target_queue, ops_filter, where_filter, redact_fields.
- Catalog: `CollectionDescriptor.subscriptions: Vec<SubscriptionDescriptor>` persistido (additive ADR 0011).
- Auto-create queue: se `TO` omitido, cria `<collection>_events` com mode `FANOUT` (per #283).
- Cycle detector: rejeita DDL que cria ciclo `users → audit → users`.
- Loop prevention: `CREATE QUEUE foo WITH EVENTS` rejeitada explicitamente.
- 1 conformance case + parser tests.
- Sem emissão de evento ainda — slice 2 entrega.

## Acceptance criteria

- [ ] `CREATE TABLE users WITH EVENTS` parse OK; queue `users_events` auto-criada com `FANOUT`.
- [ ] `CREATE TABLE users WITH EVENTS TO audit_log` parse OK; aponta pra queue existente ou cria.
- [ ] `WITH EVENTS REDACT (email, phone)` persiste redact list.
- [ ] `WITH EVENTS (INSERT, UPDATE)` persiste ops_filter.
- [ ] `WITH EVENTS WHERE status = 'active'` persiste where filter.
- [ ] `CREATE QUEUE foo WITH EVENTS` retorna erro: "queues cannot have event subscriptions".
- [ ] DDL circular rejeitada com erro: "subscription would create cycle".
- [ ] ALTER variants funcionam: `ENABLE EVENTS`, `DISABLE EVENTS`.
- [ ] Conformance: 6 casos pinned.
- [ ] Sem regressão.

## Blocked by

- #285
