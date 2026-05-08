# Cache: finalizar split blob/cache.rs + blob/mod.rs re-exports [BLOCKED]

GitHub: https://github.com/reddb-io/reddb/issues/226

Labels: enhancement, ready-for-agent, blocked

GitHub issue number: #226

## Status

Blocked. Kept out of Ralph's top-level queue.

## Original GitHub Body

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

- [ ] `blob/mod.rs` contém apenas `pub mod ...; pub use ...;` (sem lógica).
- [ ] Todos os símbolos exportados em `cache/mod.rs:49-55` continuam acessíveis.
- [ ] Nenhum consumidor externo do crate precisou mudar import.
- [ ] `wc -l blob/*.rs`: nenhum arquivo >2000 linhas.
- [ ] `cargo test -p reddb-server` passa sem regressão.
- [ ] Testes de `BlobCache` (sharding, round-trip, L1↔L2) migrados para `blob/cache.rs::tests`.

## Blocked by

- #223
- #224
