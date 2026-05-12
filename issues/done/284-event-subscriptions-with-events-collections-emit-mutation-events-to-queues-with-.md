# Event subscriptions: WITH EVENTS — collections emit mutation events to queues with redaction + tenant isolation [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/284

Labels: enhancement

GitHub issue number: #284

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como autor de pipeline integrando RedDB com sistemas downstream (Snowflake, Elasticsearch, audit log central, cache invalidation, webhook fan-out), hoje **não tenho como rastrear mutações em uma collection** sem modificar a aplicação. Cada operação INSERT/UPDATE/DELETE é fire-and-forget — o engine não notifica.

Workarounds atuais:
- **Polling** — query periódica `WHERE updated_at > last_check` em cada consumer. Custa CPU, perde DELETEs, latência alta.
- **Aplicação dual-write** — código de aplicação escreve no RedDB e separadamente publica no Kafka/RabbitMQ. Acoplamento, transação distribuída quebrável.
- **Trigger externo** — não há mecanismo trivial.

CDC já existe internamente (`replication/cdc.rs`) — usado para replicar primary→replica via WAL. Mas é seam interno, não exposto para usuários.

## Solution

Promover CDC interno a feature de produto: **collections emitem eventos para queues em mutações**, com configuração declarativa.

```sql
-- Habilitação simples (auto-cria queue users_events FANOUT)
CREATE TABLE users (...) WITH EVENTS;

-- Apontar para queue específica + redaction
CREATE TABLE users (...)
  WITH EVENTS TO compliance_audit
  REDACT (email, phone);

-- Múltiplas subscriptions na mesma collection
ALTER TABLE users
  ADD SUBSCRIPTION analytics
  TO snowflake_sink
  WHERE active = true;

-- Filtrar operações
CREATE TABLE users (...) WITH EVENTS (INSERT, UPDATE) TO audit;

-- Backfill explícito (não automático)
EVENTS BACKFILL users TO compliance_audit WHERE created_at > '2026-01-01';

-- Status
EVENTS STATUS users;
```

Funciona em todos os data models: tables, documents, vectors, graphs, timeseries, kv. Queues recebem mas não emitem (loop prevention).

## User Stories

1. Como autor de pipeline, quero `CREATE TABLE users WITH EVENTS` e ter eventos automaticamente disparando para `users_events` queue, sem código de aplicação extra.
2. Como autor de audit, quero `WITH EVENTS TO audit_log REDACT (email, phone)` e ter PII removida do payload no producer side, antes de chegar à queue, para compliance sem post-processing.
3. Como autor de pipeline downstream (Snowflake), quero consumir `<collection>_events` com payload `{op, before, after, id, lsn, ts, tenant}` para sync incremental sem polling.
4. Como autor de pipeline, quero **múltiplas subscriptions** na mesma collection (audit + analytics + cache) para diferentes propósitos com policies diferentes.
5. Como tenant `acme`, quero certeza absoluta que meus eventos **nunca** chegam ao tenant `globex`, mesmo com configuração errada — engine força tenant scope mandatoriamente.
6. Como root operator com `cluster:admin` + capability `events:cluster_subscribe`, quero criar subscription cross-tenant (auditoria central de todos tenants).
7. Como autor de aplicação multi-tenant, quero que `WITH EVENTS` em DDL idêntica em 2 tenants gere 2 subscriptions independentes (mesmo padrão das tables — DDL = template, scoped per tenant).
8. Como autor de subscription, quero filtrar operações: `WITH EVENTS (INSERT, UPDATE) TO audit` ignora DELETEs, para reduzir noise.
9. Como autor de subscription, quero filtro WHERE: `WITH EVENTS WHERE status = 'active' TO active_audit` para não inundar a queue com rows irrelevantes.
10. Como engenheiro de SRE, quero que `INSERT` síncrono **nunca** seja bloqueado por queue cheia (default outbox path). A única exceção é se operador setou `runtime.events.delivery_mode = sync` globalmente.
11. Como engenheiro de SRE, quero ver `outbox_lag_ms` e `drain_rate_eps` em métricas Prometheus, para alertar quando consumer atrasa.
12. Como engenheiro de SRE, quero que se queue de destino encher, o outbox segure (não perca eventos) e após N retries o evento vá para `<queue>_outbox_dlq` para inspeção manual.
13. Como autor de consumer, quero garantia de ordering per-collection — UPDATE da row 42 chega antes do segundo UPDATE da row 42 — para downstream estado consistente.
14. Como autor de consumer, quero `event_id` determinístico (BLAKE3 de `collection|id|lsn|op`) para idempotência: se replay/redelivery acontece, dedup é trivial.
15. Como autor de consumer downstream com schema rígido, quero saber quando uma `ALTER TABLE` adiciona coluna nova — engine emite `OperatorEvent` audit alertando, para downstream se preparar.
16. Como autor de migration de subscription, quero que `EVENTS BACKFILL users TO audit` enfileire eventos `synthetic: true` para todas linhas existentes, para downstream começar do zero sem perder histórico.
17. Como autor de subscription, quero distinguir `synthetic: true` (backfill) de eventos real-time, para tratamento downstream diferente se aplicável.
18. Como autor de policy, quero certeza que developer com `select` em `users` mas sem `read` na queue `users_audit` não pode `QUEUE READ users_audit` — queue policy gates separately.
19. Como autor de policy, quero que `WITH EVENTS TO public_audit` sem REDACT em collection com `DENY select ON column:users.email` policy emita warning na DDL, para alertar sobre potencial vazamento.
20. Como autor de DDL, quero `CREATE QUEUE foo WITH EVENTS` rejeitada, porque queues nunca emitem eventos (loop prevention).
21. Como autor de DDL, quero engine detectar ciclo: `users → audit → users` rejeita por subscription cycle.
22. Como autor de bulk INSERT (1M rows), quero opção `INSERT INTO ... SUPPRESS EVENTS` para skip events em loads gigantes (e fazer backfill seletivo depois).
23. Como autor de TRUNCATE, quero **um** evento `truncate` (não N delete events), para downstream poder fazer single op ao invés de processar 1M deletes.
24. Como autor de DROP TABLE, quero **um** evento `collection_dropped`, queue preservada para drenagem dos pendentes; subscription removida.
25. Como engineer de replication, quero certeza que replicas **não** disparam eventos ao aplicar WAL (engine marca `from_replication: true`), para evitar duplicação primary+replica.
26. Como engineer de SDK Java/Python/Go, quero documentation clara explicando "subscribe esta queue, processe eventos em ordem, ack" — pattern padrão com pseudo-code.
27. Como autor de tests, quero conformance suite com casos pinned: cada operation × cada model × cada redact pattern.
28. Como autor de tests, quero property tests provando idempotency: replay 100x não duplica eventos no consumer.
29. Como engenheiro mantendo o sistema, quero que `EVENTS STATUS users` mostre subscription, queue, lag, dlq_count.
30. Como autor de DDL, quero `ALTER TABLE foo DISABLE EVENTS` que para a subscription mas preserva queue e mensagens pendentes.

## Implementation Decisions

### Módulos novos

- **`runtime/events/subscription_registry.rs`** — persiste tuples `(source, target_queue, ops_filter, where_filter, redact_fields, tenant_scope, mode)`. Reativo: catalog state + WAL durabilidade.
- **`runtime/events/outbox.rs`** — WAL extension. Cada commit que toca event-enabled collection escreve outbox entry com payload pronto. Drain worker reads + pushes to target queue. Backpressure: bloqueia se queue full, retry N vezes, então DLQ.
- **`runtime/events/cycle_detector.rs`** — DAG do subscription graph. Rejeita DDL que cria ciclo. O(V+E) per ALTER.
- **`runtime/events/payload_builder.rs`** — gera JSON envelope `{op, collection, id, ts, lsn, tenant, before, after}`. Aplica REDACT (strip fields). Computa `event_id = BLAKE3(...)`.
- **`runtime/events/backfill.rs`** — comando `EVENTS BACKFILL`. Lê snapshot, batch enfileira como synthetic events. Idempotente.

### Modificações

- **Parser** (`storage/query/parser/`) — adiciona tokens `EVENTS`, `REDACT`, `SUBSCRIPTION`, `BACKFILL`, `SUPPRESS`. DDL: `CREATE TABLE ... WITH EVENTS [(<ops>)] [TO <queue>] [REDACT (<fields>)] [WHERE <pred>]`. ALTER: `ENABLE EVENTS`, `DISABLE EVENTS`, `ADD SUBSCRIPTION`, `DROP SUBSCRIPTION`. Comandos: `EVENTS BACKFILL`, `EVENTS STATUS`.
- **Catalog** (`catalog.rs`) — `CollectionDescriptor.subscriptions: Vec<SubscriptionDescriptor>`.
- **Mutation pipeline** (`runtime/mutation.rs`, `impl_dml.rs`) — após mutation succeed, lookup subscriptions pra collection, monta payload, escreve outbox entry. Skip se `from_replication: true` ou `SUPPRESS EVENTS`.
- **Replication path** (`replication/`) — marca contexto `from_replication: true` para gate.
- **Auth** (`auth/`) — capability nova `events:cluster_subscribe`. DDL gate: `select` source + `write` target.

### Queue auto-creation

- `WITH EVENTS` sem `TO` cria queue `<collection>_events` com mode `FANOUT` (per PRD #283).
- Se queue já existe com mesmo nome: subscription aponta pra ela (mode preservado, mas validação: se mode != FANOUT, warning).

### Config global (runtime.events.*)

- `delivery_mode` = `outbox|sync` (default `outbox`).
- `outbox_warn_bytes` = 1 GiB.
- `outbox_max_bytes` = 10 GiB.
- `drain_max_retries` = 5.
- `drain_retry_base_ms` = 500.
- `cycle_detection_enabled` = true.

### Schema event payload

```json
{
  "event_id": "blake3:...",
  "op": "update",
  "collection": "users",
  "id": 42,
  "ts": 1715200000000,
  "lsn": 98234,
  "tenant": "acme",
  "synthetic": false,
  "before": {"name": "Alice"},
  "after": {"name": "Alice Costa"}
}
```

REDACT remove fields de `before` e `after`. `id` é PK user se declarada; senão synthetic.

### Operações cobertas

| Op | Comportamento | Configurável |
|---|---|---|
| INSERT (single + multi) | 1 evento per row | `SUPPRESS EVENTS` |
| UPDATE / DELETE | 1 evento per row afetada | `SUPPRESS EVENTS` |
| TRUNCATE | 1 evento `truncate` | não |
| DROP COLLECTION | 1 evento `collection_dropped` | não |
| DDL (ALTER/CREATE INDEX) | NÃO dispara (audit log normal) | — |
| Replicação | NÃO dispara (`from_replication: true` gate) | não |
| AUTO EMBED vector creation | NÃO dispara separadamente (incluído no insert event) | — |
| BACKFILL | sim, `synthetic: true` | sim |

## Testing Decisions

### Princípio

Testar comportamento externo: "INSERT row → 1 evento na queue com payload esperado" / "UPDATE 1000x mesma row → 1000 eventos em ordem por lsn" / "queue cheia + N retries → evento em DLQ". Não testar internals do outbox/cycle detector.

### Cobertura

- **Conformance corpus** — cada DDL form pinned: `CREATE WITH EVENTS`, `WITH EVENTS TO`, `REDACT`, `WHERE`, ALTER variants, `EVENTS BACKFILL`, `EVENTS STATUS`.
- **Integration tests** — round-trip INSERT/UPDATE/DELETE/TRUNCATE/DROP em cada model (table/document/vector/graph/timeseries/kv) → validar event payload + ordering.
- **Tenant isolation tests** — INSERT em tenant A nunca aparece em queue do tenant B.
- **Cycle detection** — DDL circular rejeitada com mensagem clara.
- **Loop prevention** — `CREATE QUEUE foo WITH EVENTS` rejeitada.
- **REDACT tests** — fields strippados, payload sem PII.
- **Outbox backpressure** — queue cheia → drain bloqueia, após N retries vai pra DLQ.
- **Replication gate** — INSERT em primary + replica não duplica eventos.
- **Backfill** — `EVENTS BACKFILL` enfileira N eventos com `synthetic: true`.
- **Idempotency** — replay 100x não duplica downstream.
- **Property tests** — `proptest` sobre subscription configs random, prova invariantes (tenant scope nunca quebrado, ordering per-collection preservado).

### Prior art

- `replication/cdc.rs` — CDC infra existente.
- `storage/queue/consumer_group.rs` — queue groups testados.
- `tests/conformance/` (PRD #229) — corpus framework.

## Out of Scope

- **Schema versioning automático** (default a) — opt-in via `WITH SCHEMA_VERSION` cabe em PRD futura.
- **Strict mode subscription** — bloqueia inserts até reconfig após ALTER. Compliance edge case.
- **Cross-region replication-aware events** — events vão pra queue local; replica reproduz queue. Não cross-region direct.
- **Webhook delivery direta** (sem queue intermediária) — fora.
- **Avro / Protobuf payload encoding** — JSON only MVP.
- **External Kafka delivery** (skip queue, push direto) — futuro PRD.
- **Subscription replay com offset external** — consumer mantém seu próprio offset hoje.
- **Per-subscription policies** complexas além de REDACT — RLS-style on event payload é PRD futura.
- **Event compaction** (Kafka log compaction) — fora.

## Further Notes

- Origem: grilling session 2026-05-08 com 15 perguntas resolvidas.
- ADR não obrigatória — decisões seguem padrões indústria (CDC + outbox + DLQ). Se durante implementação aparecer trade-off não-óbvio, criar ADR 0012+.
- **Bloqueada por PRD #283** (Queue modes) — `FANOUT` precisa existir antes de auto-event queue funcionar.
- Pareada arquiteturalmente com #239 (catalog), #240 (column policy), #272 (AI batching) — todas surgiram da auditoria sistemática do produto.
- Estimativa total: 8-12 dias se sequencial.
- Após to-issues: ~12-15 slices.
- Sem `git rebase` durante implementação (regra global).
