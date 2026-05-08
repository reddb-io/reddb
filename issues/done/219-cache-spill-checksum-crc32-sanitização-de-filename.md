# Cache: spill checksum crc32 + sanitização de filename [AFK]

## Parent

#217

## What to build

`spill.rs:519,604` usa `data.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32))` como checksum — qualquer permutação de bytes passa, não detecta corrupção. Trocar por `crc32` (já disponível em `engine::crc32`, usado em `blob.rs:1604`).

`spill.rs:507` `format!("{}-{}.spill", name, pid)` não sanitiza `name` — input com `../` pode escapar do diretório de spill. Adicionar helper `sanitize_spill_name` que valida que o caminho final permanece dentro do diretório base.

Bumpar versão do header on-disk de v1 para v2; reader aceita ambos por uma janela (decidir tamanho da janela durante triagem).

## Acceptance criteria

- [ ] `spill::write` usa `engine::crc32` no novo header v2.
- [ ] `spill::read` aceita v1 (legacy fold) e v2 (crc32).
- [ ] Teste: round-trip OK retorna bytes idênticos.
- [ ] Teste: mutar 1 byte → `read` retorna erro.
- [ ] Teste: permutar 2 bytes → `read` retorna erro (caso v1 passaria).
- [ ] `name = "../foo"`, `name = "/etc/passwd"`, `name = "a/b"` → arquivo final permanece dentro do diretório base ou erro.
- [ ] Sem regressão em `cargo test -p reddb-server`.

## Blocked by

None - can start immediately
