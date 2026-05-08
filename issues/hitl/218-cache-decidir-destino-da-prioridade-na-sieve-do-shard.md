# Cache: decidir destino da prioridade na SIEVE do Shard [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/218

Labels: enhancement, ready-for-human

GitHub issue number: #218

## Status

Requires human/security review. Kept out of Ralph's AFK queue.

## Original GitHub Body

## Parent

#217

## What to build

Decisão arquitetural HITL: a SIEVE do `Shard` em `blob.rs:1030` introduz `has_lower_priority_unvisited` (linhas 1057/1092), violando o invariante #1 do `cache/README.md` ("uma única eviction signal"). Decidir entre:

(a) **Remover prioridade** — Shard usa SIEVE puro, igual `sieve.rs::PageCache`. Simplifica e respeita o README.
(b) **Formalizar exceção** — manter prioridade documentando o motivo (ex: tiering L1/L2 exige), e reescrever invariante #1 com a exceção explícita.

Output: comentário no issue com decisão + uma linha registrável em `cache/README.md`. Não há código nesta fatia — apenas decisão que destrava slices 7 e 9.

## Acceptance criteria

- [ ] Decisão registrada em comentário do issue: (a) remover ou (b) formalizar.
- [ ] Se (a): plano de remoção citando `has_lower_priority_unvisited` e callsites.
- [ ] Se (b): rascunho da nova redação do invariante #1 do `cache/README.md`.
- [ ] Justificativa cita workload-alvo (blob L1 com tiers vs page cache homogêneo).

## Blocked by

None - can start immediately
