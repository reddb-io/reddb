# null: AdminIntentLog: extend docs/operations/logging.md with admin intent journal section

## Parent

#207

## What to build

Estender `docs/operations/logging.md` (criado em #204) com nova seção "Admin intent journal" cobrindo:
- Propósito (control-plane recovery, complementa audit_log + red-slow.log)
- Localização do sink (`red-admin-intent.log` no mesmo dir do audit_log)
- Schema JSON line (id/op/phase/ts/actor/args/progress/summary)
- Como inspecionar com jq (3+ exemplos: list begins, list unfinished, count by op)
- DanglingAdminIntent OperatorEvent — quando dispara, severity forensic-only, response esperado
- Crescimento esperado (~50KB/dia)
- Linux-first caveat (O_APPEND POSIX 4KB, macOS PIPE_BUF=512 documentado)
- Cross-link pra docs/operations/replication.md (uma vez #208 wired)

NÃO criar arquivo standalone (memory rule: no standalone ADRs/docs, embed nos topic docs existentes).

## Acceptance criteria

- [ ] docs/operations/logging.md ganha seção "Admin intent journal"
- [ ] Schema JSON documentado com exemplo de cada phase
- [ ] 3+ exemplos de jq query funcionais
- [ ] Cross-link pra docs/operations/replication.md
- [ ] Linux-first explicitly stated com caveat macOS

## Blocked by

- #208
