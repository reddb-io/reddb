# Events, CDC, and transport `rid` vocabulary sweep [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Make event payloads, CDC, MCP/gRPC/SDK result shapes, and public docs use the ADR 0019 vocabulary consistently after the row, document/KV, and graph tracers land.

## Acceptance criteria

- [ ] Event payloads identify changed items with `rid`, `collection`, and `kind`.
- [ ] CDC payloads identify changed items with `rid`, `collection`, and `kind`.
- [ ] MCP/gRPC/SDK result shapes touched by item identity use `rid`.
- [ ] Public docs touched by these surfaces use RedDB ID, `rid`, item, and item `kind`.
- [ ] Regression tests cover at least one row event and one non-row item event/payload path.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 468-document-kv-rid-envelope-tracer.md
- 469-graph-rid-from-rid-to-rid-tracer.md

## Progress note (2026-05-16)

Implemented:
- `delete_event_payload` (`crates/reddb-server/src/runtime/mutation.rs`) was
  hardcoding `kind = "row"` on every delete. Added a `kind: &str` parameter
  and derived it in `emit_delete_events_for_collection` from
  `contract.declared_model` (Document → `document`, Kv/Vault → `kv`,
  otherwise `row`). Insert/update events already used
  `event_item_kind_for_entity`; this fixes the last hardcoded path so
  delete events satisfy the ADR 0019 vocabulary.
- Added regression test `row_delete_event_payload_uses_public_item_identity`
  in `tests/e2e_events_cdc_rid.rs`. Existing tests
  `row_event_payload_uses_public_item_identity` (insert) and
  `cdc_changes_payload_uses_public_item_identity_for_kv` (non-row)
  already cover the other acceptance bullets.

Surface audit (no changes needed):
- Insert/update event payloads — `rid`/`collection`/`kind` already wired
  via `insert_event_item_identity` + `event_item_kind_for_entity`.
- CDC payloads — `ChangeRecord::to_json_value` and `public_item_kind`
  already emit `rid`/`collection`/`kind`.
- HTTP /changes docs (`docs/api/http.md:790`) already use the new vocab.
- Events docs (`docs/data-models/events.md`) already use `rid`/`kind`/
  `collection`; no `entity_id`/`entity_kind` references in docs/data-models.
- gRPC `EntityReply.id` field number stays the same; `entity_json`
  already carries the public `rid` envelope. Renaming the proto field
  name `id` → `rid` is a wider breaking change that affects
  drivers/python and drivers/js generated stubs and is intentionally
  deferred from this slice.

Blocker:
- Could not run `cargo check` / `cargo test --test e2e_events_cdc_rid`
  or any `git` command in this loop — every Bash call requesting
  `cargo`, `git add`, `git status`, `git commit`, or `git mv` was
  rejected with "requires approval". Changes are present on disk
  (mutation.rs, tests/e2e_events_cdc_rid.rs, this file moved to
  `issues/blocked/`) but uncommitted. Next iteration should re-run
  with permissions for cargo + git and commit if tests pass.
