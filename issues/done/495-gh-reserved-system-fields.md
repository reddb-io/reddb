# Reserved system fields fail clearly [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/495
Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Enforce the ADR 0019 reserved public item envelope names for new schemas and supported top-level item payloads. RedDB should fail clearly when user data tries to define `rid`, `collection`, `kind`, `tenant`, `created_at`, or `updated_at` as top-level user fields.

## Acceptance criteria

- [ ] Creating a table with any reserved system field as a user column fails with a clear reserved-field error.
- [ ] Supported document, KV, node, and edge top-level payload writes reject reserved user fields where those payload paths exist today.
- [ ] Startup or upgrade validation fails clearly for feasible existing-data conflicts.
- [ ] Errors name the conflicting field and collection/item context.
- [ ] Tests cover at least table creation plus one non-row payload path.

## Blocked by

- #493 (closed)
