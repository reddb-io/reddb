# Graph node and edge compound updates [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

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

- 469-graph-rid-from-rid-to-rid-tracer.md
- 471-postgres-compatible-math-functions.md
- 472-compound-assignment-row-updates.md
- 474-explicit-update-target-parser-validation.md

## Progress note (2026-05-16)

Static review: implementation appears complete via blocked-by issues.

- Target gate up front: `ensure_graph_identity_update_target_allowed` rejects `rid`/`label`/`from_rid`/`to_rid` (and `from`/`to`) on `UPDATE … NODES|EDGES` regardless of WHERE match (`crates/reddb-server/src/runtime/impl_dml.rs:1132`, `:1747`, `:1785`).
- Per-entity guard mirrors it post-resolve: `ensure_graph_identity_update_allowed` (`:1575`, `:1761`).
- Compound `+=`/`-=`/`*=`/`/=`/`%=` math + abort errors flow through the shared `evaluate_compound_update_assignment` / `apply_compound_numeric_op` path delivered by #472, so atomic-failure semantics carry over to graph targets.
- ADR 0019 contract: `update_patch_path_for_entity` routes `node_type` and `weight` to top-level structural paths instead of the dynamic-`fields.*` map (`:1838`–`:1860`); other mutable columns/properties land in `fields.*` as documented.
- Graph WHERE shape: `dml_target_scan` walks NODES/EDGES with the documented top-level shape (`crates/reddb-server/src/runtime/dml_target_scan.rs`); RETURNING uses graph-aware post-image snapshots via `graph_update_returning_snapshots` (`:1184`, `:2126`).
- RLS hooks apply uniformly to `UPDATE` regardless of `target` (`:1139`).
- Tests: `tests/e2e_graph_compound_updates.rs` covers all six AC items — compound NODES SET, compound EDGES SET (incl. weight), `node_type` mutation, identity/topology rejection, atomic abort on bad input, and RLS-scoped graph update.

Blocker: this AFK loop instance cannot invoke `cargo test`, `pnpm test`, or `git` (every command above an `echo` requires interactive approval in this environment). Cannot verify-then-move-to-done. Next loop iteration with command approval should:

1. Run `cargo test --test e2e_graph_compound_updates`.
2. If green, `git mv issues/476-graph-node-edge-compound-updates.md issues/done/` and commit.
