# Ordered multi-model update batches [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Extend ordered update batch semantics from rows to documents, KV, graph nodes, and graph edges. Each target should support `ORDER BY` with `LIMIT`, top-level order fields only, and implicit `rid ASC` tie-breaking.

## Acceptance criteria

- [ ] `DOCUMENTS`, `KV`, `NODES`, and `EDGES` updates accept `ORDER BY ... LIMIT`.
- [ ] `ORDER BY` without `LIMIT` is rejected for each target.
- [ ] Expression and nested-path ordering are rejected in this slice.
- [ ] Ties are broken by implicit `rid ASC` when `rid` is absent.
- [ ] Tests cover at least one ordered batch per non-row target.

## Blocked by

- 473-ordered-row-update-batches.md
- 475-document-kv-compound-updates.md
- 476-graph-node-edge-compound-updates.md

## Resolution note (2026-05-16)

All acceptance criteria satisfied by infrastructure landed via #473 (rows),
#475 (document/KV compound updates) and #476 (graph compound updates). The
UPDATE parser and runtime are target-agnostic for ORDER BY/LIMIT handling:

- `parse_update_query` (`crates/reddb-server/src/storage/query/parser/dml.rs:316-432`)
  parses `ORDER BY ... LIMIT` for every `UpdateTarget` (Rows, Documents, Kv,
  Nodes, Edges), runs `validate_update_order_by` to reject expression /
  nested-path order terms, errors when `ORDER BY` lacks `LIMIT`, and appends
  the implicit `rid ASC` tie-breaker via `update_order_by_mentions_rid`.
- `execute_update_inner_tracked` (`crates/reddb-server/src/runtime/impl_dml.rs:1212-1242`)
  routes every target through `ordered_update_target_ids`, which sorts the
  scanned ids by the validated order clauses (with rid tie-break) before
  applying the limit cap.
- `update_order_value` (`crates/reddb-server/src/runtime/impl_dml.rs:2600-2616`)
  resolves top-level fields against `EntityData::Row` (documents stored via
  `create_document` flatten body keys into `row.named` at
  `crates/reddb-server/src/application/ports_impls_entity.rs:2756-2762`; KV
  rows already use named `key`/`value`) and graph node/edge property maps via
  `runtime_any_record_from_entity_ref`.

Coverage lives in `tests/e2e_ordered_multimodel_update_batches.rs`: ordered
batches for documents, KV, nodes, edges; per-target rejection of missing
LIMIT, expression order, and nested-path order; implicit `rid ASC`
tie-break for documents. Row coverage stays in
`tests/e2e_ordered_row_update_batches.rs`.

No new code required — closing as already implemented.
