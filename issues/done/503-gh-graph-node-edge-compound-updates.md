# Graph node and edge compound updates [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/503

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Make explicit graph node and edge update targets work end to end with compound assignment, `WHERE`, `RETURNING`, atomicity, immutable graph identity/topology fields, and available policy hooks.

## Acceptance criteria

- [ ] `UPDATE <graph> NODES SET <property> += ... WHERE ... RETURNING ...` works for mutable top-level node fields/properties.
- [ ] `UPDATE <graph> EDGES SET weight += ... WHERE ... RETURNING ...` works for mutable edge fields/properties.
- [ ] Mutating `rid`, `label`, `from_rid`, or `to_rid` is rejected where applicable.
- [ ] Node `node_type` mutation follows the ADR 0019 contract.
- [ ] Edge `weight` mutation follows the ADR 0019 contract.
- [ ] Graph update `WHERE` sees the documented node/edge shape.
- [ ] Tests cover positive node and edge updates, immutable field rejection, and atomic failure.

## Blocked by

- #497
- #494
- #499
- #501
