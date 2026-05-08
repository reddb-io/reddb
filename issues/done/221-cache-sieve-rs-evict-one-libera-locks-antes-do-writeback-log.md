# Cache: sieve.rs evict_one libera locks antes do writeback + log de poison [AFK]

## Parent

#217

## What to build

`sieve.rs:540-560` chama `self.writer.write_page(&key_clone, &entry.value)` segurando write-lock de `entries` + `slots` — toda inserção concorrente bloqueia pela duração da escrita de página.

Mover writeback para fora dos locks: extrair o `(key, value)` do entry escolhido, liberar locks, executar `write_page`. Manter invariante: o entry só é considerado "evicted" depois do writeback bem-sucedido.

Adicional (mesma fatia, mesmo arquivo): `sieve.rs:22-35` `recover_read_guard`/`recover_write_guard` ignora `PoisonError` silenciosamente. Adicionar `tracing::warn!` (ou logger equivalente) com nome do lock antes de prosseguir.

## Acceptance criteria

- [ ] `evict_one` não segura `entries`/`slots` write-lock durante chamada a `writer.write_page`.
- [ ] Invariante preservada: falha no writeback não deixa cache em estado inconsistente (entry permanece ou é descartado atomicamente).
- [ ] `recover_*_guard` loga warn estruturado com identificador do lock em recovery de poison.
- [ ] Teste de contenção: writer instrumentado para bloquear N ms; insert paralelo completa em ≪ N ms (threshold conservador, ex: < 10% de N).
- [ ] Teste: panic em thread A com lock segurado → thread B em `recover_*_guard` produz log warn observável.
- [ ] Sem regressão em testes existentes de `sieve.rs`.

## Blocked by

None - can start immediately
