# Reserved system fields fail clearly [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Enforce the ADR 0019 reserved public item envelope names for new schemas and supported top-level item payloads. RedDB should fail clearly when user data tries to define `rid`, `collection`, `kind`, `tenant`, `created_at`, or `updated_at` as top-level user fields.

## Acceptance criteria

- [x] Creating a table with any reserved system field as a user column fails with a clear reserved-field error.
- [x] Supported document, KV, node, and edge top-level payload writes reject reserved user fields where those payload paths exist today.
- [x] Startup or upgrade validation fails clearly for feasible existing-data conflicts.
- [x] Errors name the conflicting field and collection/item context.
- [x] Tests cover at least table creation plus one non-row payload path.

## Blocked by

- 466-rid-row-envelope-tracer.md

## Resolution

Implemented in commit dc33c1fe `fix(schema): reject reserved public item fields`.

- Added `crates/reddb-server/src/reserved_fields.rs` central guard for the ADR 0019 envelope names.
- Wired validation into runtime DDL/DML and the application entity write paths (documents, KV, graph nodes/edges).
- Persisted-table contracts re-validated during metadata initialization in `storage/unified/devx/reddb/impl_metadata.rs`.
- Coverage in `tests/e2e_reserved_system_fields.rs`.
