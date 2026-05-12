# Bind support in K / LIMIT / OFFSET / MIN_SCORE / SEARCH SIMILAR TEXT clauses [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/361

Labels: needs-triage

GitHub issue number: #361

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Extend the binder to accept parameters in additional clauses beyond VALUES / WHERE:

- `K $N` (vector top-k)
- `LIMIT $N` / `OFFSET $N`
- `MIN_SCORE $N`
- `SEARCH SIMILAR TEXT $N USING <provider>` (text param for auto-embedded search)
- `PROBES $N` (IVF)
- Any other clause that today accepts a literal but should accept a parameter

Each clause requires a typed binder context (integer for K/LIMIT/OFFSET/PROBES, float for MIN_SCORE, text for SIMILAR TEXT). Mismatches return typed errors.

## Acceptance criteria

- [ ] All listed clauses accept `$N` parameters and reject wrong types with typed errors.
- [ ] `db.query('SEARCH SIMILAR $1 IN embeddings K $2 MIN_SCORE $3', [vec, 5, 0.7])` works end-to-end.
- [ ] `db.query('SEARCH SIMILAR TEXT $1 COLLECTION docs USING openai', [text])` works.
- [ ] Tests cover each clause with both correct and incorrect parameter types.

## Blocked by

- #355
