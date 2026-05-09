# Column policy: conformance + property test suite (path × model × action) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/271

Labels: needs-triage

GitHub issue number: #271

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#240

## What to build

Suite exaustiva provando que cada combinação `(path, model, action)` aplica column gate.

End-to-end:
- Conformance corpus: 1 caso fixo por combinação relevante. ≥30 casos.
- Property test (`proptest`): gera policies aleatórias + queries → prova que negados nunca aparecem no resultset.
- Performance bench: gate adiciona ≤5% latência em SELECT trivial.
- CI test que valida coverage matrix completa.

## Acceptance criteria

- [ ] Conformance corpus tem ≥30 casos (path × model × action).
- [ ] Property test gera 256 casos default sem violation.
- [ ] Bench compara antes/depois: < 5% regression em hot path SELECT.
- [ ] CI gate: coverage matrix completa (sem gaps marcados como `not_covered`).

## Blocked by

- #265
- #266
- #267
- #268
- #269
