# Cache: extrair blob/shard.rs aplicando decisão #218 + atualizar README invariante [AFK]

## Parent

#217

## What to build

Terceira fatia do split. Extrair `Shard` (`blob.rs:903-1097`) para `blob/shard.rs` aplicando a decisão tomada em #218:

- (a) **Se #218 decidiu remover prioridade:** eliminar `has_lower_priority_unvisited` (linhas 1057/1092), Shard fica SIEVE puro consistente com `sieve.rs::PageCache`.
- (b) **Se #218 decidiu formalizar:** preservar prioridade, mas com nome explícito (ex: `TieredSieveShard`) e comentário citando o invariante.

Reescrever invariante #1 do `cache/README.md` para refletir a realidade:
- Se (a): manter "uma única eviction signal" e adicionar nota "Shard do blob também respeita".
- Se (b): registrar exceção documentada com motivo.

Esta fatia **não** muda algoritmo de O(n) para O(1) — isso é #BLOCKED_BY_THIS+1.

## Acceptance criteria

- [ ] `blob/shard.rs` contém `Shard` isolado, com testes próprios.
- [ ] Decisão de #218 aplicada e visível no diff.
- [ ] `cache/README.md` invariante #1 reescrito conforme decisão.
- [ ] Sem mudança de comportamento observável (mesmos testes de eviction passam).
- [ ] `cargo test -p reddb-server` passa.

## Blocked by

- #218
- #222
