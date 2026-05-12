# DDL: auth enforcement — drop + truncate privileges wire ao executor [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/309

Labels: enhancement

GitHub issue number: #309

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

GRANT DROP / GRANT TRUNCATE hoje aceita o vocabulário mas não enforces. Esta slice wire enforcement real no executor.

End-to-end:
- Pre-execute hook em DROP/TRUNCATE handlers chama `IamPolicyEngine::check(principal, action, resource)`.
- `action`: `drop` ou `truncate`.
- `resource`: `collection:<schema>.<name>` (ou similar — alinhar com vocabulário existente em `policies.md`).
- Sem privilege → 403 com mensagem clara.
- Polymorphic DROP: privilege check feito após resolver (sabe a collection target, valida).
- DROP COLLECTION resolver erro = before privilege check (collection não existe → 404, não 403).
- Audit log via existing AuditLogger.

## Acceptance criteria

- [ ] Principal sem `drop` policy em `collection:foo` → DROP foo retorna 403.
- [ ] Principal com `GRANT DROP ON collection:foo` → DROP foo succeed.
- [ ] Wildcard: `GRANT DROP ON collection:public.*` aplica a todas em schema public.
- [ ] Mesma cobertura para TRUNCATE.
- [ ] DROP COLLECTION polymorphic respeita privilege da collection target (não "drop em todas").
- [ ] Audit log emite entry com principal, action, resource, decision.
- [ ] Conformance corpus: 4 casos (allow, deny, wildcard allow, missing privilege error).

## Blocked by

- #307
- #308
