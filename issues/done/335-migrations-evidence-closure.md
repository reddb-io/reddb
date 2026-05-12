# Migrations evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/335

Labels: enhancement

GitHub issue number: #335

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Verify or split the remaining migration evidence gaps for bulk apply, branch-scoped schema conflict behavior, and migration clippy cleanup. The slice should prove observable migration behavior through SQL/runtime outcomes or create narrow follow-up issues for any missing behavior.

Covers: #16, #21, #24

User stories covered: 4, 5, 6

## Acceptance criteria

- [x] APPLY MIGRATION * has end-to-end evidence for applying all pending migrations in dependency order.
- [x] Branch-scoped migration conflict behavior has evidence through VCS/schema merge behavior or a follow-up issue for the missing contract.
- [x] The impl_migrations redundant-closure cleanup is either proven by current lint/code evidence or marked superseded with the current module name.
- [x] The evidence report no longer marks #16, #21, or #24 as partial without a final disposition.

## Closure notes

- Added `tests/e2e_migrations_bootstrap.rs` coverage for `APPLY MIGRATION *` applying five pending migrations across dependent and independent chains, verifying applied status, VCS commit hashes, and visible SQL effects.
- Split the missing branch-scoped `MigrationConflict` merge behavior to local follow-up #346 because the current workspace has dependency/runtime evidence but no migration-specific branch conflict contract.
- Marked #24 superseded by the current module path `crates/reddb-server/src/runtime/impl_migrations.rs`.
- Regenerated both evidence report JSON artifacts with #16 confirmed, #21 split, and #24 superseded.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
