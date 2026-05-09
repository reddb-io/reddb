# Events: docs data-models/events.md + conformance corpus completo [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/305

Labels: needs-triage

GitHub issue number: #305

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Documento canônico + conformance suite cobrindo todas as 30 user stories.

End-to-end:
- `docs/data-models/events.md` novo:
  - Visão geral: collections emitem eventos pra queues
  - Quickstart: `WITH EVENTS` com auto-queue + custom queue
  - Payload spec
  - REDACT, filters, multi-subscription
  - Backpressure + DLQ semantics
  - Tenant isolation
  - Cycle prevention
  - Idempotência via event_id
  - Schema evolution semantics
  - BACKFILL pattern
  - Comparação com Postgres logical replication / Mongo change streams / Kafka Connect
- Conformance corpus: ≥30 casos cobrindo cada user story.
- Cross-reference em `red-schema.md` (red.subscriptions).
- Cross-reference em `policies.md` (REDACT + queue policies).

## Acceptance criteria

- [ ] `docs/data-models/events.md` cobre todas user stories.
- [ ] Tabela comparativa com 3 sistemas externos.
- [ ] ≥30 conformance cases.
- [ ] Cross-references aplicadas.
- [ ] Quickstart roda em fixture cluster.

## Blocked by

- #294
- #295
- #296
- #297
- #298
- #299
- #300
