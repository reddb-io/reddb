# Events: outbox path + INSERT events end-to-end (THE BIG WIN) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/292

Labels: needs-triage

GitHub issue number: #292

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

**A slice que entrega o feature.** Implementa todo o caminho do INSERT até a queue: WAL outbox + drain worker + payload builder + push.

End-to-end:
- **WAL outbox extension**: cada commit que toca event-enabled collection escreve outbox entry no WAL com payload pronto.
- **Drain worker**: background thread/task que lê outbox sequencial, push para target queue. Bloqueia se queue full (backpressure por ora simples; DLQ vem em slice 10).
- **Payload builder**: gera `{event_id, op, collection, id, ts, lsn, tenant, before, after}` JSON.
- **`event_id` determinístico**: BLAKE3(`collection || id || lsn || op`).
- **Per-collection ordering**: outbox preserva LSN order.
- **Tenant isolation**: payload tenant scope vem do EffectiveScope da mutation.
- **Replication gate**: se mutation vem de WAL replay (replica), engine NÃO emite (`from_replication: true` flag no contexto).

Por simplicidade, esta slice cobre só **INSERT**. UPDATE/DELETE virão na slice 3.

Bench: INSERT 1000 rows em event-enabled collection mede latência adicional vs disabled. Alvo: ≤10% overhead em commit.

## Acceptance criteria

- [ ] `INSERT INTO users (...) VALUES (...)` em event-enabled `users` produz evento em `users_events` com payload completo.
- [ ] Multi-row INSERT produz N eventos em ordem.
- [ ] Bulk INSERT (1000 rows) produz 1000 eventos sem perder nenhum.
- [ ] Replicação aplicando WAL no replica não duplica eventos.
- [ ] Ordering per-collection respeitado (LSN ascendente).
- [ ] `event_id` determinístico: same input → same id.
- [ ] Bench: ≤10% overhead vs INSERT em collection sem events.
- [ ] Integration test cobre payload completo (campos esperados).
- [ ] Conformance: 2 casos (single-row, multi-row).

## Blocked by

- #291
