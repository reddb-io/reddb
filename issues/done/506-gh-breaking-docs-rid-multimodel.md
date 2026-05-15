# Breaking docs and migration guide for `rid` and multi-model updates [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/506

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Sweep public docs and examples for ADR 0019 after implementation slices land. The docs should teach `rid`, RedDB ID, item, item `kind`, `from_rid`, `to_rid`, compound assignment, math functions, and multi-model update targets as the canonical surface. The migration guide should call out breaking removals and reserved-field conflicts.

## Acceptance criteria

- [x] SQL reference documents `rid`, item envelope fields, item kinds, and update targets.
- [x] Data model docs for tables, documents, KV, and graphs use the new vocabulary.
- [x] API/SDK/MCP/gRPC docs use `rid` and avoid older public identifier aliases.
- [x] Graph docs use `from_rid` and `to_rid`.
- [x] Update docs cover compound assignment, math function examples, `RETURNING`, `ORDER BY ... LIMIT`, and atomic failure behavior.
- [x] Migration guide lists removed aliases, reserved system fields, and user-action requirements.
- [x] Changelog/release notes mark the change as breaking.

## Blocked by

- #493
- #495
- #496
- #497
- #498
- #494
- #499
- #500
- #501
- #502
- #503
- #504
- #505
