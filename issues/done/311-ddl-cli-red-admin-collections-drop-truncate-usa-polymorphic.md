# DDL: CLI — red admin collections drop/truncate (usa polymorphic) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/311

Labels: enhancement

GitHub issue number: #311

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

Sub-comandos novos em `red admin collections` que executam DROP/TRUNCATE polymorphic via SQL nativo internamente.

End-to-end:
- `red admin collections drop <name> [--if-exists]`
- `red admin collections truncate <name> [--if-exists]`
- Internamente: roda `DROP COLLECTION <name>` ou `TRUNCATE COLLECTION <name>` via runtime existente.
- Output: tabela ANSI com confirmação + métricas (entities_count antes da op, etc).
- `--json` para scripts.
- Confirmação interativa para DROP (segurança), `--yes` para skip.
- TRUNCATE não pede confirmação (operação reversível menos catastrófica).
- Documentado em `docs/cli/red-admin.md` (criar se não existir, ou estender).

## Acceptance criteria

- [ ] `red admin collections drop users` → confirmação interativa → success.
- [ ] `red admin collections drop users --yes` → sem confirmação.
- [ ] `red admin collections truncate users` → success direto (sem confirmação).
- [ ] `--if-exists` funcional.
- [ ] `--json` emite JSON estruturado para scripts.
- [ ] Snapshot tests do output formatado.
- [ ] Documentado em `docs/cli/red-admin.md`.

## Blocked by

- #307
- #308
