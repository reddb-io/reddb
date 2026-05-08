# Column policy: wire enforcement em INSERT column list [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/268

Labels: needs-triage

GitHub issue number: #268

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#240

## What to build

Wire column gate em INSERT.

End-to-end:
- `INSERT INTO users (id, name, email) VALUES (...)` chama `gate(principal, Insert, ["users.id", "users.name", "users.email"])`.
- Coluna negada: 403 (write-side).
- Bulk INSERT (multi-row VALUES): gate uma vez por column list, não por row.

## Acceptance criteria

- [ ] Policy `DENY insert ON column:users.email` rejeita `INSERT INTO users (..., email) VALUES ...` com 403.
- [ ] INSERT sem coluna negada: passa normal.
- [ ] Bulk INSERT: gate executado uma vez por query (não N vezes).
- [ ] Conformance corpus: 4+ casos.

## Blocked by

- #264
