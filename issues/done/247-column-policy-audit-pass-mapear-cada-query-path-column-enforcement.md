# Column policy: audit pass — mapear cada query path × column enforcement [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/247

Labels: enhancement

GitHub issue number: #247

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#240

## What to build

**HITL slice.** Auditoria sistemática do runtime para descobrir, em cada query path, se column-deny policy é aplicada hoje ou não. Resultado: documento `docs/security/column-enforcement-coverage.md` com tabela exaustiva.

Por que HITL: as conclusões da auditoria afetam priorização das slices 17-21. Engenheiro humano deve revisar achados e confirmar ordem de wiring antes de prosseguir.

End-to-end:
- Mapear cada query path em `runtime/query_exec/`, `runtime/dml.rs`, `runtime/impl_*.rs`.
- Para cada path × ação (select, insert, update, delete) × modelo (table, document, queue, vector, graph, timeseries, kv): documentar se column gate é chamado, qual chamada, e qual cobertura (todas as colunas? só projetadas? wildcard?).
- Comparar contra `docs/security/policies.md` claims atuais.
- Identificar discrepâncias (ex: doc promete enforcement, código não faz).
- Output: tabela markdown em `docs/security/column-enforcement-coverage.md` com colunas (path, model, action, status, gap, priority).
- Output secundário: lista priorizada das slices subsequentes (17-21) e ordem recomendada.

## Acceptance criteria

- [ ] `docs/security/column-enforcement-coverage.md` criado com tabela exaustiva de paths × models × actions.
- [ ] Cada combinação tem status: `enforced` / `partial` / `missing` / `not_applicable`.
- [ ] Cada `partial` ou `missing` tem nota explicando o gap específico.
- [ ] Output recomenda ordem das próximas slices baseado em frequência de uso e severidade do gap.
- [ ] Engenheiro humano revisou e aprovou conclusões antes de marcar como complete.

## Blocked by

None - can start immediately
