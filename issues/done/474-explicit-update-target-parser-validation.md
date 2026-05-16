# Explicit update target parser and validation [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Parse and validate explicit item-kind update targets: `ROWS`, `DOCUMENTS`, `KV`, `NODES`, and `EDGES`. Omitted target remains rows. Collection model compatibility should be checked before mutation.

## Acceptance criteria

- [ ] Parser accepts `UPDATE <collection> ROWS|DOCUMENTS|KV|NODES|EDGES SET ...`.
- [ ] Parser preserves omitted-target row update behavior.
- [ ] Runtime validation rejects incompatible target/model combinations before mutation.
- [ ] Graph collections accept both `NODES` and `EDGES`.
- [ ] Generic or mixed collections can accept explicit item-kind targets where supported.
- [ ] Cross-kind update forms such as `UPDATE FROM ANY` remain rejected.
- [ ] Parser and runtime tests cover positive and negative target cases.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 468-document-kv-rid-envelope-tracer.md
- 469-graph-rid-from-rid-to-rid-tracer.md

## Progress note (2026-05-16)

Duplicate of done issue `issues/done/501-gh-explicit-update-target-parser-validation.md`.
Implementation already on main:
- Parser: `crates/reddb-server/src/storage/query/parser/dml.rs` accepts `UPDATE <coll> ROWS|DOCUMENTS|KV|NODES|EDGES SET ...`, preserves omitted-target row behavior.
- Runtime validation: `crates/reddb-server/src/runtime/impl_dml.rs` rejects incompatible target/model combos before mutation.
- Tests: `tests/e2e_explicit_update_targets.rs` covers positive/negative target cases including graph NODES/EDGES and rejected cross-kind forms.

Bash + git mv blocked in this session (permission denied for `cargo test` and `git mv`). Could not run feedback loops or move file to `issues/done/`. Leaving note; next iteration should `git mv` to `issues/done/`.
