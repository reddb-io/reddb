# Cache: BlobCache::open retorna Result em I/O de boot [AFK]

## Parent

#217

## What to build

`BlobCache::new` em `blob.rs:2107` faz `expect("open blob-cache L2")` — config L2 inválida derruba o servidor inteiro em I/O recuperável de boot. Idem nas linhas 2667, 2675, 2689. Converter para `Result` propagado.

Plano:
- `BlobCache::new` continua infalível para configs **sem** L2 (compat).
- Novo `BlobCache::open_with_l2(config) -> Result<BlobCache, CacheError>` para configs com L2.
- Estender `CacheError` existente — não criar novo enum.
- Callers em `cache/mod.rs` e no boot do server passam a tratar o `Err`.

## Acceptance criteria

- [ ] Nenhum `expect()` em path de boot do BlobCache (varredura: `rg 'expect' blob.rs` — só em caminhos provados unreachable).
- [ ] `BlobCache::open_with_l2` retorna `Err(CacheError::...)` em L2 path inválido sem panic.
- [ ] Boot do server loga erro estruturado e falha graciosamente em vez de panic.
- [ ] Teste: L2 path em diretório read-only → `Err`, processo vivo.
- [ ] Teste: L2 path com control sidecar corrompido → `Err`.
- [ ] API pública existente em `cache/mod.rs` preservada (sem breaking change para callers que não usam L2).

## Blocked by

None - can start immediately
