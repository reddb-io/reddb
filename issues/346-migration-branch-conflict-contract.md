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

- [ ] `CREATE MIGRATION` on a feature branch is visible only on that branch until merged, or the unsupported branch-scope contract is explicitly rejected in docs.
- [ ] Merging two branches with migrations touching the same collection or column surfaces a migration-specific conflict and blocks the merge until resolved.
- [ ] The conflict names both migrations and the collection or column they conflict on.
- [ ] Adding an explicit `DEPENDS ON` or equivalent ordering resolution lets the merge complete.
- [ ] Disjoint migration branches merge without migration conflicts.
- [ ] Public runtime/VCS tests cover overlapping and non-overlapping migration branches.

## Blocked by

None.
