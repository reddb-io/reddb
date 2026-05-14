# Make Documents a first-class CRUD model [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/452

## Parent

#449

## What to build

Make Documents usable as a first-class RedDB model across the public runtime,
SQL, HTTP, and persistence surfaces. Users must be able to create a document
collection, insert JSON-like documents, fetch/list/delete them through the
document APIs, query them as enriched rows, and reopen persisted data without
losing document behavior.

This is unblocked because #450 is closed.

Keep the slice minimal but real: start by reproducing the current
`CREATE DOCUMENT` / document CRUD gap with failing tests, then implement the
smallest behavior needed to pass those tests without refactoring unrelated
models.

## Acceptance criteria

- [x] `CREATE DOCUMENT <name>` succeeds and registers or creates a document
      collection without a `NOT_YET_SUPPORTED` error.
- [x] `INSERT INTO <collection> DOCUMENT (...) VALUES (...) RETURNING *`
      returns stable document identity and inserted fields.
- [x] HTTP document insert, get-by-id, list or filter, and delete paths work
      with documented JSON envelopes if the HTTP surface already advertises
      them; if names differ, use the existing documented route names and record
      them in the issue notes.
- [x] Document rows are queryable through SQL/runtime as enriched rows.
- [x] Document CRUD persists across database reopen.
- [x] Runtime, HTTP, and persistence tests cover realistic document scenarios
      using nested JSON, arrays, and scalar fields.

## Verification

- `rtk cargo fmt --check`
- `rtk cargo test` for the focused document tests you add
- `rtk make check`
- Run JS smoke only if the `red` binary exists or is built as part of the
  workflow; otherwise record why it was skipped.

## Guardrails

- Preserve existing user changes in other worktrees.
- Do not broaden the document model beyond what the tests need.
- Do not silently change table, graph, KV, or timeseries behavior.
- If an advertised HTTP document route is missing, add the smallest route that
  matches the public documentation and cover it with a test.

## Blocked by

None. #450 is closed.

## Completion notes

- Implemented `CREATE DOCUMENT` via the existing keyed collection contract path.
- `INSERT INTO ... DOCUMENT ... RETURNING *` now returns `red_entity_id` plus the persisted flattened document fields.
- Added `GET /collections/{name}/entities/{id}` for documented entity CRUD symmetry; document list/filter uses the existing `/collections/{name}/scan` and SQL `/query` surfaces.
- HTTP document create now accepts the documented `metadata` object.

## Verification

- `rtk cargo fmt --check`
- `rtk cargo test --test e2e_documents_first_class_crud`
- `rtk make check`
- `rtk pnpm test` skipped by the script because `target/debug/red` was not present.
- `rtk pnpm typecheck` failed because the workspace still resolves the placeholder `tsc` package instead of a configured TypeScript compiler.
