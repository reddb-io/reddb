# Parser coverage uplift A: queue.rs (45→90%) + timeseries.rs (36→90%) [AFK]

## Parent

#227

## What to build

Subir cobertura de linha dos dois arquivos com maiores lacunas:

- `parser/queue.rs`: 45.32% → ≥90%. Cobrir: PEEK/LEN/PURGE, RPOP/LPOP, GROUP commands (ACK/NACK/EXTEND), DLQ flows, MAX_ATTEMPTS, IF NOT EXISTS, CREATE QUEUE com todas combinações de WITH clauses.
- `parser/timeseries.rs`: 36.16% → ≥90%. Cobrir: CREATE TIMESERIES com retention, downsample, materialized views, todos `time_bucket` granularities, INSERT com timestamp/timestamp_ns/time aliases, FROM cpu_metrics WHERE metric=, SELECT com window functions.

Estratégia: identificar branches descobertas via `cargo llvm-cov --html` (gera relatório HTML por linha), escrever testes pinned em `parser/tests.rs` ou no conformance corpus. Cada novo teste exercita um caminho que faltava.

## Acceptance criteria

- [ ] `cargo llvm-cov --lib -p reddb-server --summary-only -- 'storage::query::parser'` reporta `queue.rs` ≥90% lines.
- [ ] Idem `timeseries.rs` ≥90% lines.
- [ ] Total geral de testes não regride (3315 atual + novos).
- [ ] Cada teste novo tem nome descritivo apontando para o branch que cobre (ex: `queue_push_with_dlq_and_max_attempts`).
- [ ] Workflow #228 (CI coverage) reporta os novos números no PR.
- [ ] Sem regressão em `cargo test -p reddb-server`.

## Blocked by

- #228
- #229
