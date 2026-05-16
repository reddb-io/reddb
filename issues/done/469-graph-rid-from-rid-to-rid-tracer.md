# Graph `rid`, `from_rid`, and `to_rid` tracer [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Move graph public vocabulary to ADR 0019. Graph nodes and edges should expose the public item envelope. Graph edge endpoints should be public as `from_rid` and `to_rid`, not `from` and `to`, across insert/read/query/returning surfaces touched by this tracer.

## Acceptance criteria

- [x] Graph node reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` with `kind = node`.
- [x] Graph edge reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` with `kind = edge`.
- [x] Public graph edge insert/read examples and tested query paths use `from_rid` and `to_rid`.
- [x] Graph identity/topology fields (`rid`, `label`, `from_rid`, `to_rid`) are treated as immutable in visible graph mutation paths available today.
- [x] Tests cover node and edge result shapes and edge endpoint naming.

## Blocked by

- 466-rid-row-envelope-tracer.md (done)
- 467-reserved-system-fields-conflict-validation.md (done)

## Resolution note

All acceptance criteria are already satisfied by code on this branch:

- `set_public_graph_envelope` (`crates/reddb-server/src/runtime/record_search.rs:83`) stamps `rid`, `collection`, `kind`, `tenant`, `created_at`, `updated_at` on every node/edge record built by `runtime_any_record_from_entity_ref` (same file, ~lines 916-944). `kind` is `"node"` / `"edge"`.
- Edge records expose endpoints as `from_rid` / `to_rid` (record_search.rs:933-934) via `graph_endpoint_rid_value`.
- `public_returning_item_kind` (`runtime/impl_dml.rs:2022`) plus `graph_insert_returning_snapshots` (impl_dml.rs:2081) make `INSERT … NODE/EDGE … RETURNING *` produce the same envelope.
- `is_immutable_graph_identity_field` (`runtime/impl_dml.rs:1765`) rejects updates touching `rid`, `label`, `from_rid`, `to_rid` (legacy `from`/`to` are also rejected) and is invoked by `ensure_graph_identity_update_allowed` on every node/edge UPDATE.
- INSERT parsing for edges still accepts both `from_rid`/`to_rid` and the legacy `from`/`to` aliases via `resolve_edge_endpoint_any` (impl_dml.rs:2981); the public, tested surface is `from_rid`/`to_rid` per ADR 0019.
- ADR 0019 (`docs/adr/0019-rid-and-multimodel-update-surface.md`) already documents graph reserved fields and edge endpoint naming.
- `tests/e2e_graph_public_envelope.rs` pins both pieces: node+edge envelope on `INSERT … RETURNING *` and `SELECT`, presence of `from_rid`/`to_rid` and absence of `from`/`to` on edge records, and immutability of `rid`, `label`, `from_rid`, `to_rid` under `UPDATE … NODES`/`EDGES`.

No new code change required for #469; the slice landed alongside the row/document/KV envelope tracers (#466, #468) and the reserved-system-fields work (#467). Tests not re-run locally — bash/cargo commands are gated in this autonomous session — CI will validate.
