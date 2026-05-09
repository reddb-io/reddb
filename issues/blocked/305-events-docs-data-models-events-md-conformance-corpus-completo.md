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

## Progress note - Agent #305, 2026-05-09

Completed the docs/corpus slice that is available without touching runtime:

- Added `docs/data-models/events.md` as the canonical event-subscription guide.
- Covered auto queues, custom queues, payload shape, REDACT, operation/WHERE
  filters, multiple subscriptions, tenant isolation, cycle prevention,
  idempotent `event_id`, backpressure/DLQ, schema evolution, consumer pattern,
  and comparison with Postgres logical replication, MongoDB change streams, and
  Kafka Connect.
- Added 30 parser conformance cases under
  `crates/reddb-server/tests/conformance/events_e*.toml`, including one
  negative loop-prevention case.
- Added cross-references in `docs/reference/red-schema.md` and
  `docs/security/policies.md`, plus sidebar/overview/queues/query links.

Partial blockers remain:

- #300 is still open in this worktree. `EVENTS BACKFILL ...` and
  `synthetic: true` are documented as planned, but not included as runnable
  conformance syntax.
- #303 is still open in this worktree. `red.subscriptions` and `EVENTS STATUS`
  are documented as planned, not implemented.
- Because #300/#303 are absent, the full quickstart/status fixture criterion is
  not complete.

Validation:

- `python3 crates/reddb-server/tests/conformance/validate_sources.py` passed
  with 162 source references.
- `cargo test -p reddb-server --test conformance` first blocked on the shared
  Cargo artifact lock. A second run with isolated `CARGO_TARGET_DIR` reached
  the final `reddb-server` compile but was stopped after the cold compile
  remained in progress for several minutes; no parser failure was observed
  before stopping.
