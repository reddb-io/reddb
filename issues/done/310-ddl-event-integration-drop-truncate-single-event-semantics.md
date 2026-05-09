# DDL: event integration — DROP/TRUNCATE single-event semantics [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/310

Labels: needs-triage

GitHub issue number: #310

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

Wire DROP/TRUNCATE no path de eventos (pareando com #284 slice 4 / #302):
- DROP em event-enabled collection → 1 evento `collection_dropped` antes da collection ser removida. Subscription removida do catalog. Queue de eventos preservada com modo "drained-only" (consumer pode ler pendentes; novos pushes rejeitados).
- TRUNCATE em event-enabled collection → 1 evento `truncate` com `entities_count`. **Não** N delete events.

End-to-end:
- Hook em handler de DROP: enumera subscriptions ativas, emite `collection_dropped`, depois remove subscription, depois remove collection.
- Hook em handler de TRUNCATE: emite `truncate` event com count, depois zera rows.
- Queue lifecycle: queue de eventos sobrevive ao DROP da source. Operador deleta queue manualmente após drain (`DROP QUEUE users_events` separado).
- Coordenação com #302 — payload format (`op: "truncate"|"collection_dropped"`) já especificado lá.

## Acceptance criteria

- [ ] `DROP TABLE users` em event-enabled users → 1 evento `{op: "collection_dropped", collection, ts, lsn, tenant, final_entities_count}` em `users_events`. Subscription removida. Queue preservada.
- [ ] `TRUNCATE TABLE users` (1M rows) em event-enabled users → 1 evento `{op: "truncate", collection, ts, lsn, tenant, entities_count: 1_000_000}`. Não 1M delete events.
- [ ] DROP/TRUNCATE em collection sem event subscription → comportamento normal, sem evento.
- [ ] Conformance corpus: 4 casos (DROP+events, TRUNCATE+events, DROP-no-events, TRUNCATE-no-events).

## Blocked by

- #307
- #308
- #302
