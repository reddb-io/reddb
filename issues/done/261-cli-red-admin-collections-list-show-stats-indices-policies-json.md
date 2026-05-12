# CLI: red admin collections {list,show,stats} + indices/policies + --json [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/261

Labels: enhancement

GitHub issue number: #261

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Sub-comandos novos em `red admin` que executam internamente o SQL nativo. Output formatado humano + flags de export.

Comandos:
- `red admin collections list [--type table|queue|...] [--include-internal]`
- `red admin collections show <name>` — agrega `SHOW SCHEMA + INDICES + POLICIES + STATS` em saída multi-section
- `red admin collections stats [<name>]`
- `red admin indices list [--collection X]`
- `red admin policies list [--collection X]`
- `red admin query "<SQL>"` — passthrough genérico

Flags globais: `--json`, `--csv`, `--no-color`, `--limit N`.

## Acceptance criteria

- [ ] `red admin collections list` produz tabela ANSI colorida.
- [ ] `red admin collections list --json | jq '.[].name'` extrai nomes.
- [ ] `red admin collections show users` mostra schema + indices + policies + stats em seções.
- [ ] `red admin indices list --collection users` filtra por collection.
- [ ] `--no-color` desliga ANSI (CI-safe).
- [ ] Internamente: cada comando faz 1+ query SQL nativa via runtime existente.
- [ ] Snapshot tests do output formatado (fixture cluster).
- [ ] Documentado em `docs/cli/red-admin.md`.

## Blocked by

- #244
- #254
- #255
- #256
- #257
