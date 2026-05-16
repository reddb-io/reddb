# Document and KV `rid` envelope tracer [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Extend the public item envelope and `rid` vocabulary to document and KV item paths. Document query/read/returning surfaces should expose `kind = document`; KV query/read/returning surfaces should expose `kind = kv`.

## Acceptance criteria

- [ ] Document reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [ ] KV reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [ ] Document item `kind` is `document`.
- [ ] KV item `kind` is `kv`.
- [ ] Public JSON/API/SDK-visible paths touched by document and KV reads use `rid`.
- [ ] Tests cover document and KV query results through the public query path.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 467-reserved-system-fields-conflict-validation.md

## Progress (2026-05-16)

Audited public read paths for documents and KV against the rid envelope contract:

- SELECT * (record_search.rs): `set_public_row_envelope` already calls `public_row_kind`, which returns `kv` / `document` / `row` based on row contents. ✔
- INSERT ... RETURNING * (impl_dml.rs `build_returning_result` + `public_returning_row_kind`): emits `rid, collection, kind, tenant, created_at, updated_at` with kv/document detection. ✔
- KV GET (impl_kv.rs): result schema already includes `rid, collection, kind, tenant, created_at, updated_at`; kind comes from `keyed_model_name(model)` (= `kv` for KV). ✔
- UPDATE ... RETURNING * (dml_target_scan.rs `entity_row_snapshot`): hardcoded `kind = "row"`. **Fixed** in this iteration: now reuses `row_item_kind` so kv/document rows surface their correct kind in UPDATE RETURNING envelopes.
- Test coverage: `tests/e2e_rid_row_envelope.rs::document_and_kv_reads_expose_public_rid_envelope` exercises INSERT RETURNING, SELECT, and KV GET for both `document` and `kv` items.

### Blocker

The autonomous loop environment refused `cargo test`, `git status`, `git add`, and `git commit` (Bash permission denials). The doc/kv envelope code change is in place and looks complete on inspection, but cannot be verified or committed from this iteration. Next iteration: run `cargo test --test e2e_rid_row_envelope` and `pnpm typecheck`/`pnpm test` (if applicable), then commit `crates/reddb-server/src/runtime/dml_target_scan.rs` and move this file to `issues/done/`.
