# Parser conformance: CI workflow para coverage report (warn-only) [AFK]

## Parent

#227

## What to build

Workflow GitHub Actions que roda em cada PR, executa `cargo llvm-cov` filtrado para `storage::query::parser` e `storage::query::lexer`, e comenta no PR uma tabela com line coverage por arquivo + delta vs main. Sem bloquear merge — apenas reporta.

Output esperado em comentário do PR:
```
| Arquivo | Coverage | Delta | Threshold (90%) |
| parser/queue.rs | 47.2% | +1.9 | ❌ -42.8 |
| parser/timeseries.rs | 36.1% | 0.0 | ❌ -53.9 |
| ...
```

## Acceptance criteria

- [ ] `.github/workflows/parser-coverage.yml` criado, dispara em `pull_request` que toca `crates/reddb-server/src/storage/query/**` ou `Cargo.lock`.
- [ ] Workflow instala `cargo-llvm-cov` e roda `cargo llvm-cov --lib -p reddb-server --summary-only -- 'storage::query::parser'`.
- [ ] Workflow extrai coverage por arquivo de `parser/*` e `lexer.rs`, formata tabela markdown, posta como PR comment (atualiza comentário existente em vez de duplicar).
- [ ] Marcador visual: ✅ se ≥90%, ❌ caso contrário. Não falha o job.
- [ ] Delta vs main calculado quando possível (baseline: workflow paralelo no push para main que salva snapshot).
- [ ] Documentado em `crates/reddb-server/AGENTS.md` ou em comment do workflow: bloquear merge é PRD futuro.

## Blocked by

None - can start immediately
