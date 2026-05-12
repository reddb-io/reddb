# Column policy: update docs/security/policies.md (remove disclaimer) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/270

Labels: enhancement

GitHub issue number: #270

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#240

## What to build

Atualizar documentação para refletir que column-level enforcement está completo.

End-to-end:
- Remover disclaimer "where wired" / "use views, RLS workarounds" de `docs/security/policies.md:89-91`.
- Atualizar tabela de coverage em `docs/security/permissions.md` para mostrar column-level como feature plena.
- Atualizar `docs/security/column-enforcement-coverage.md` (criado em #247) com estado final.
- Adicionar exemplos canônicos: PII deny via column policy, sem precisar VIEW.

## Acceptance criteria

- [ ] Disclaimer removido de `policies.md`.
- [ ] Tabela coverage atualizada em `permissions.md`.
- [ ] Coverage doc reflete enforcement real (post #265-269).
- [ ] Exemplos canônicos com column-deny.
- [ ] Cross-references para slices implementadas.

## Blocked by

- #265
- #266
- #267
- #268
- #269
