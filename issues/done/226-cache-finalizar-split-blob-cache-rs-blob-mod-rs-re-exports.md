# Cache: finalizar split blob/cache.rs + blob/mod.rs re-exports [AFK]

## Parent

#217

## What to build

Última fatia do split. Mover `BlobCache` orquestrador (sharding, API pública, `BlobCachePut`, `BlobCacheHit`, etc) para `blob/cache.rs`. `blob/mod.rs` fica reduzido a re-exports.

Após esta fatia, `blob/` tem:
- `mod.rs` (apenas re-exports)
- `config.rs`
- `entry.rs`
- `shard.rs`
- `l2.rs`
- `cache.rs`

Nenhum arquivo isolado >2000 linhas.

## Acceptance criteria

- [x] `blob/mod.rs` contém apenas `pub mod ...; pub use ...;` (sem lógica).
- [x] Todos os símbolos exportados em `cache/mod.rs:49-55` continuam acessíveis.
- [x] Nenhum consumidor externo do crate precisou mudar import.
- [x] `wc -l blob/*.rs`: nenhum arquivo >2000 linhas.
- [x] `cargo test -p reddb-server` passa sem regressão.
- [x] Testes de `BlobCache` (sharding, round-trip, L1↔L2) migrados para `blob/cache.rs::tests`.

## Completion note

Moved the BlobCache orchestrator into `crates/reddb-server/src/storage/cache/blob/cache.rs`,
left `blob/mod.rs` as module declarations and re-exports, and moved the BlobCache test module
under `blob/cache/tests.rs` so the test path is `blob::cache::tests` while every `blob/*.rs`
file remains below 2000 lines. Full `cargo test -p reddb-server` still has unrelated failures
in admin CAS auth, coercion_spine, and telemetry tests; blob cache tests pass.

## Blocked by

- #223
- #224
