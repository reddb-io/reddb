# null: LeaseTimerWheel: replace lease_loop polling with edge-triggered timer wheel

## What to build

Substituir `crates/reddb-server/src/runtime/lease_loop.rs` (polling com `thread::sleep(interval)` baseado em `ttl_ms / 3`) por uma data structure de timer wheel. Lease insert chama `wheel.schedule(lease_id, expiry_ts)`. Wheel mantém slots por bucket de tempo; worker thread acorda exatamente quando próximo bucket vence, fires expiry handler pra leases naquele slot, e dorme até o próximo bucket. Sem CPU em leases idle.

Mantém comportamento externo: o que `lease_loop` faz hoje (limpeza de lease expirado) continua acontecendo, só muda quando/como o wake acontece.

Pattern: top-half/bottom-half. Insert é top-half (push slot, return). Worker é bottom-half (drena slot atual, processa expiry). Já é a arquitetura que `tracing_appender::NonBlocking` e `AsyncPromotionPool` usam — peer pattern.

## Acceptance criteria

- [ ] `LeaseTimerWheel` deep module em `crates/reddb-server/src/runtime/lease_timer_wheel.rs` com API `schedule(id, expiry)`, `cancel(id)`, `run_until_shutdown()`
- [ ] `lease_loop.rs` substituído (ou reescrito como thin wrapper sobre o wheel)
- [ ] `cargo test -p reddb-server lease` passa
- [ ] Bench microbenchmark: 10k idle leases consumem <0.1% CPU (vs current poll cycle waking up every `ttl_ms/3`)
- [ ] Latência de expiry handler dispatch dentro de N ms da expiry real (N = bucket granularity, configurable, default 100ms)
- [ ] Sem regressão em testes existentes de lease lifecycle

## Blocked by

None — can start immediately.
