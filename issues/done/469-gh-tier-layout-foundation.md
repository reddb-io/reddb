# Tier layout foundation: ADR-0018 + layout module + tier config types [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/469

Labels: enhancement, ready-for-agent

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#467

## What to build

Estabelecer a fundação arquitetural do layout tiered: ADR-0018 descrevendo o contrato `minimal/standard/performance/max`, módulo de layout (mapeamento puro `data_path -> paths` por tier, sem I/O nos getters, apenas `ensure_dirs()` toca disco), e tipos de configuração (`StorageLayout` enum + `LayoutOverrides` struct + expansão determinística de preset -> toggles).

Nenhum callsite muda nesta slice. O módulo é introduzido e testado em isolamento; integração vem na slice #2.

## Acceptance criteria

- [ ] ADR-0018 publicado em `docs/adr/` cobrindo os 4 tiers, link cruzado com ADR-0003.
- [ ] Módulo de layout exposto com API funcional pura sobre `&Path` cobrindo todos os paths derivados de cada tier.
- [ ] Tipos `StorageLayout` e `LayoutOverrides` serde-friendly, com default = `standard`.
- [ ] Expansão preset -> toggles é determinística e testada com tabela exaustiva.
- [ ] Testes unitários sem I/O cobrem todos os tiers + overrides + casos de path (com/sem extensão, parent relativo/absoluto).
- [ ] `cargo check` e suite atual passam sem regressão.

## Blocked by

None - can start immediately

