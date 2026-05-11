# Migration branch conflict contract [AFK]

GitHub: local follow-up from reddb-io/reddb#335

Labels: needs-triage

GitHub issue number: #346

## Parent

#335 (https://github.com/reddb-io/reddb/issues/335)

## What to build

Implement or explicitly re-scope branch-scoped migration merge conflict behavior. Current code has migration dependency ordering and VCS conflict materialization, but no `MigrationConflict` contract tying migrations on divergent branches to schema-aware merge blocking.

Covers: remaining branch-scoped acceptance from #21

## Acceptance criteria

- [x] `CREATE MIGRATION` on a feature branch is visible only on that branch until merged, or the unsupported branch-scope contract is explicitly rejected in docs.
- [x] Merging two branches with migrations touching the same collection or column surfaces a migration-specific conflict and blocks the merge until resolved, or the unsupported branch-scope contract is explicitly rejected in docs.
- [x] The conflict names both migrations and the collection or column they conflict on, or the unsupported `MigrationConflict` contract is explicitly rejected in docs.
- [x] Adding an explicit `DEPENDS ON` or equivalent ordering resolution lets the merge complete.
- [x] Disjoint migration branches merge without migration conflicts, or the unsupported branch-scope contract is explicitly rejected in docs.
- [x] Public runtime/VCS tests cover the explicitly rejected branch-scoped migration visibility contract.

## Closure notes

- Re-scoped branch-scoped migration visibility as unsupported in `docs/migrations/overview.md` and `docs/migrations/vcs-integration.md`.
- Added `scripts/migration_branch_contract.test.mjs` to pin the docs contract that `red_migrations` is global, `MigrationConflict` is not emitted by `red vcs merge`, and explicit `DEPENDS ON` remains the supported ordering tool.
- Added `migration_registration_is_global_across_vcs_branches` in `tests/e2e_migrations_bootstrap.rs` to prove the current public runtime behavior: a migration registered on a feature branch remains observable after checkout to `main`.

## Blocked by

None.
