# Queue modes: FANOUT vs WORK as first-class queue flag [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/283

Labels: enhancement

GitHub issue number: #283

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como autor de pipeline, hoje tenho que entender o conceito de "consumer group" do Kafka pra escolher entre dois patterns fundamentais de queue:

- **Pub/sub (broadcast)** — cada consumer recebe todas as mensagens. Ex: notificações pra múltiplos serviços, fan-out de eventos.
- **Work queue (compete)** — consumers dividem mensagens. Ex: jobs distribuídos, tasks queue.

A queue do RedDB já tem consumer groups internamente, mas o user precisa entender o modelo Kafka-implícito: "1 group por consumer = fanout; 1 group, N consumers = work". Não é descoberto por intuição — exige ler doc avançada.

Outras DBs expõem essa escolha como conceito explícito:
- **Apache Pulsar:** `Exclusive`/`Shared`/`Failover` subscription type
- **JMS:** Topic vs Queue como primitivos diferentes
- **RabbitMQ:** "fanout exchange" vs "work queue" pattern

Falta no RedDB esse conceito explícito. E é pré-requisito para a feature de event subscriptions (PRD #B), porque queues geradas por `WITH EVENTS` precisam default `FANOUT` (cada audit/analytics/cache consumer quer todas as mensagens) — não dá pra ergonomicamente escolher isso sem promover o conceito.

## Solution

Promover o modo da queue a flag first-class na DDL:

```sql
-- Cada consumer recebe TODAS as mensagens (pub/sub)
CREATE QUEUE notifications FANOUT;

-- Consumers dividem (work queue, default)
CREATE QUEUE tasks WORK;
CREATE QUEUE tasks;  -- default = WORK

-- ALTER existente
ALTER QUEUE foo SET MODE FANOUT;
```

Internamente:
- `WORK` = todos consumers compartilham um único consumer group implícito (kafka-style compete).
- `FANOUT` = engine cria 1 consumer group implícito por consumer name (kafka-style broadcast).
- `red.collections` expõe coluna `queue_mode` para queues.
- `SHOW QUEUES` mostra modo.

Backward compat:
- Queues existentes (criadas pré-feature) assumem default `WORK`.
- Sintaxe `QUEUE GROUP CREATE ... CONSUMER ...` continua funcionando para uso avançado granular. Modo flag é o atalho ergonomic.

## User Stories

1. Como autor de notificação multi-serviço, quero `CREATE QUEUE notifications FANOUT` e ter cada consumer ouvindo todos os eventos sem decorar consumer groups, para reduzir cognitive load.
2. Como autor de pipeline de jobs, quero `CREATE QUEUE tasks WORK` (ou só `CREATE QUEUE tasks`) e ter consumers dividindo trabalho, para padrão de worker pool sem boilerplate.
3. Como operador rodando `SHOW QUEUES`, quero ver coluna `mode` mostrando `FANOUT|WORK` para auditoria de propósito da queue.
4. Como autor de migration, quero que queues existentes assumam `WORK` como default backward-compat, para não quebrar setups existentes.
5. Como autor de event subscription (#B), quero que queue auto-criada por `WITH EVENTS` tenha default `FANOUT`, porque cada subscriber espera todas as mensagens.
6. Como operador, quero `ALTER QUEUE foo SET MODE FANOUT` para mudar modo de queue existente, com warning se há consumers ativos (mudança de modo afeta delivery semantics).
7. Como engenheiro escrevendo SDK, quero que o modo seja exposto na metadata da queue (`red.collections.queue_mode`), para client-side decisions.
8. Como engenheiro mantendo conformance suite, quero casos pinned para cada modo + transição via ALTER, para que regressões sejam detectadas.
9. Como autor de doc, quero `docs/data-models/queues.md` reescrito explicando os 2 modos com exemplos canônicos, antes da seção de consumer groups (que vira power-user content).
10. Como autor de teste, quero unit tests provando que `FANOUT` entrega mesma mensagem para 3 consumers diferentes, e `WORK` distribui entre 3 consumers.

## Implementation Decisions

### Módulos / interfaces

- **`storage/queue/mode.rs` (novo)** — enum `QueueMode { Work, Fanout }`. Persistido em `QueueDescriptor` (catalog metadata).
- **`storage/queue/consumer_group.rs` (modificado)** — interpreta `QueueMode`:
  - `Work`: todos os consumers compartilham consumer group default (ID determinístico).
  - `Fanout`: cada `QUEUE READ` sem `GROUP` explícito implicitamente cria/usa um group `_consumer_<consumer_name>`.
- **`storage/query/parser/queue.rs` (modificado)** — adiciona `FANOUT|WORK` token + parsing em `CREATE QUEUE ... [FANOUT|WORK]`.
- **`runtime/impl_queue.rs` (modificado)** — DDL apply persiste mode. ALTER atualiza.
- **`red.collections` (extend, additive ADR 0011)** — coluna `queue_mode` (`text`, null para non-queue collections).

### Backward compat

- Queues existentes em catalog sem campo `queue_mode` lidos como `Work`.
- Sintaxe `QUEUE GROUP CREATE ... CONSUMER ...` continua válida — modo apenas determina default group routing quando `GROUP` é omitido em `QUEUE READ`.

### Stability policy

- `queue_mode` é stable column em `red.collections` (ADR 0011 additive).
- `CREATE QUEUE foo` sem modo continua válido (default `WORK`).

## Testing Decisions

### Princípio

Testar comportamento externo: "3 consumers em queue FANOUT recebem cada um todas as N mensagens" / "3 consumers em queue WORK distribuem N mensagens (cada consumer pega ~N/3)". Não testar implementação interna (qual consumer group ID engine gerou).

### Cobertura

- Unit tests por modo: `Work` distribui, `Fanout` broadcasts.
- Conformance corpus: `CREATE QUEUE foo FANOUT`, `CREATE QUEUE foo WORK`, `ALTER QUEUE foo SET MODE FANOUT`.
- Integration: 3 consumers em paralelo + asserts em `received_count`.
- Backward compat: queue criada antes do feature flag default `WORK`.
- ALTER mode com consumers ativos: comportamento documentado (mensagens em flight são WORK; novas usam mode novo).

## Out of Scope

- **Failover/Exclusive subscription types** (Pulsar) — fora MVP.
- **Key-Shared mode** (sticky por key) — fora.
- **Cross-region replication-aware queue mode** — fora.
- **Mode change com migration de mensagens em flight** — best effort: novas mensagens usam mode novo, em flight não migram.

## Further Notes

- Origem: grilling session 2026-05-08 sobre event subscriptions. Queue mode emergiu como pré-requisito ergonomic.
- Pareada com PRD B (Event subscriptions) — esta é blocker.
- ADR não necessária — decisão segue padrões da indústria (RabbitMQ/Pulsar), não é trade-off único.
- Estimativa: 3-5 dias se sequencial.
- Após to-issues: ~5 slices.
