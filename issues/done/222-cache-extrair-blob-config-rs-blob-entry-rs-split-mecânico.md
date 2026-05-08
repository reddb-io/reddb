# Cache: extrair blob/config.rs + blob/entry.rs (split mecânico) [AFK]

## Parent

#217

## What to build

Primeira fatia do split de `blob.rs` (5201 linhas). Mecânica, sem mudança de comportamento.

- Promover `blob.rs` para `blob/mod.rs`.
- Extrair `blob/config.rs`: `BlobCacheConfig`, builders `with_*`, defaults (`DEFAULT_BLOB_L1_BYTES_MAX` etc), constantes `METRIC_*`.
- Extrair `blob/entry.rs`: `Entry`, `L2Record`, encode/decode, helpers de checksum.
- `blob/mod.rs` re-exporta tudo via `pub use` para preservar API atual em `cache/mod.rs:49-55`.
- Mover testes relacionados junto com cada módulo.

Não tocar `Shard`, `BlobCacheL2`, ou `BlobCache` nesta fatia — fica para slices seguintes.

## Acceptance criteria

- [ ] `blob.rs` substituído por diretório `blob/` com `mod.rs`, `config.rs`, `entry.rs`.
- [ ] Todos os símbolos exportados em `cache/mod.rs:49-55` continuam acessíveis (verificar por `cargo check`).
- [ ] Nenhum consumidor externo do crate precisou mudar import.
- [ ] Testes específicos de config/entry vivem ao lado do código (`#[cfg(test)] mod tests`).
- [ ] `cargo test -p reddb-server` passa sem regressão.
- [ ] `wc -l blob/*.rs` mostra distribuição razoável (nenhum arquivo isolado >2500 linhas após esta fatia — alvo final é cumprido em slices seguintes).

## Blocked by

None - can start immediately
