# Breaking docs and migration guide for `rid` and multi-model updates [AFK]

Labels: docs, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Sweep public docs and examples for ADR 0019 after implementation slices land. The docs should teach `rid`, RedDB ID, item, item `kind`, `from_rid`, `to_rid`, compound assignment, math functions, and multi-model update targets as the canonical surface. The migration guide should call out breaking removals and reserved-field conflicts.

## Acceptance criteria

- [ ] SQL reference documents `rid`, item envelope fields, item kinds, and update targets.
- [ ] Data model docs for tables, documents, KV, and graphs use the new vocabulary.
- [ ] API/SDK/MCP/gRPC docs use `rid` and avoid older public identifier aliases.
- [ ] Graph docs use `from_rid` and `to_rid`.
- [ ] Update docs cover compound assignment, math function examples, `RETURNING`, `ORDER BY ... LIMIT`, and atomic failure behavior.
- [ ] Migration guide lists removed aliases, reserved system fields, and user-action requirements.
- [ ] Changelog/release notes mark the change as breaking.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 467-reserved-system-fields-conflict-validation.md
- 468-document-kv-rid-envelope-tracer.md
- 469-graph-rid-from-rid-to-rid-tracer.md
- 470-events-cdc-transport-rid-vocabulary-sweep.md
- 471-postgres-compatible-math-functions.md
- 472-compound-assignment-row-updates.md
- 473-ordered-row-update-batches.md
- 474-explicit-update-target-parser-validation.md
- 475-document-kv-compound-updates.md
- 476-graph-node-edge-compound-updates.md
- 477-ordered-multimodel-update-batches.md
- 478-update-target-auth-rls-events-conformance.md
