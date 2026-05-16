# Document and KV compound updates [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Make explicit document and KV update targets work end to end with compound assignment, `WHERE`, `RETURNING`, atomicity, and the available permission/RLS hooks.

## Acceptance criteria

- [ ] `UPDATE <collection> DOCUMENTS SET <field> += ... WHERE ... RETURNING ...` works for top-level document fields.
- [ ] `UPDATE <collection> KV SET value += ... WHERE key = ... RETURNING ...` works for numeric KV values.
- [ ] Document and KV updates use post-image `RETURNING`.
- [ ] Missing, null, non-numeric, division-by-zero, modulo-by-zero, and overflow failures abort the whole statement.
- [ ] Document and KV update `WHERE` see the documented top-level item shapes.
- [ ] Available authorization/RLS checks use the explicit target.
- [ ] Tests cover document and KV positive updates, invalid inputs, and atomic failure.

## Blocked by

- 468-document-kv-rid-envelope-tracer.md
- 471-postgres-compatible-math-functions.md
- 472-compound-assignment-row-updates.md
- 474-explicit-update-target-parser-validation.md

## Progress note (2026-05-16)

Static review: implementation appears complete via blocked-by issues.

- Parser: `parse_update_target` accepts `DOCUMENTS`/`KV` (`crates/reddb-server/src/storage/query/parser/dml.rs:441`).
- Target validation: `ensure_update_target_contract` enforces declared model (`crates/reddb-server/src/runtime/impl_dml.rs:2150`).
- Item-kind scan: `dml_target_scan::row_item_kind` separates rows/documents/kv (`crates/reddb-server/src/runtime/dml_target_scan.rs:329`).
- Compound math + abort errors: `evaluate_compound_update_assignment`, `apply_compound_numeric_op` (`crates/reddb-server/src/runtime/impl_dml.rs:2285`) emit "numeric field", "non-null numeric field", "existing numeric field", "division by zero", "modulo by zero", "numeric overflow" — all asserted by the test.
- RLS hook: `rls_is_enabled`/`rls_policy_filter` applied uniformly to UPDATE regardless of target (`crates/reddb-server/src/runtime/impl_dml.rs:1139`).
- Tests: `tests/e2e_document_kv_compound_updates.rs` covers all four AC items (doc compound, kv compound, atomic abort with invalid inputs, RLS scoped target).

Blocker: this AFK loop instance cannot invoke `cargo test`/`pnpm test`/`git` (all require approval in this environment). Cannot verify or commit the move-to-done. Next loop iteration with command approval should:

1. Run `cargo test --test e2e_document_kv_compound_updates` and `cargo test --test e2e_explicit_update_targets`.
2. If green, `git mv issues/475-document-kv-compound-updates.md issues/done/` and commit.
