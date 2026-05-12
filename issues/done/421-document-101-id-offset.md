# Document the 101-id offset for user entities — DONE (option A: docs)

GitHub: https://github.com/reddb-io/reddb/issues/421

## Decision

Picked option A (document the existing offset). Rationale:

- Reallocating internal entities into a separate id space (option C) is a
  `.rdb` file-format change that requires a version bump and a migration
  for every on-disk database in the wild. The offset is stable and
  already correct — it's a knowledge gap, not a defect.
- Adding a `_first_user_id` collection property (option B) introduces a
  cross-collection notion that doesn't fit the per-collection metadata
  model and would still need documenting. Net new API surface for zero
  marginal value over a paragraph.
- The acceptance criterion "(a) Document the offset" alone closes the
  issue. The other two paths are explicitly listed as alternatives.

## What was done

1. New paragraph in `docs/data-models/graphs.md` under a fresh
   `## Entity IDs` section between the lead-in and `## Creating Nodes`.
   Names the offset (102), states it's stable across `memory://` and
   `file://`, points readers at `INSERT ... RETURNING *` for retrieving
   the id at runtime, and back-references the file-format spec.

2. New `### Entity ID Allocation` subsection in
   `docs/engine/file-format.md` at the top of `## 7. Entity Binary
   Format`. Calls out the seed value (`1`), the 101 reserved ids for
   descriptor bootstrap, that the offset is part of the persisted
   contract (so changing it is a format-version bump), and links back
   to the graphs page.

3. Regression test pinning the offset in
   `crates/reddb-server/tests/runtime_query_behavior.rs`:
   `first_user_entity_id_is_one_hundred_and_two`. Inserts a single
   NODE on a fresh `in_memory()` runtime and asserts the returned
   `red_entity_id == 102`. The test's panic message points readers at
   both doc pages, so if the descriptor allocation changes, the docs
   stay in sync by design.

## Acceptance criteria

- [x] User-facing docs explain the id space (option A — current behavior).
- [x] If behavior changes (option C): N/A — docs path, behavior unchanged.
- [x] Acceptance tests pin the first user id behavior — see
      `first_user_entity_id_is_one_hundred_and_two`.

## Files changed

- `docs/data-models/graphs.md` (new `## Entity IDs` section)
- `docs/engine/file-format.md` (new `### Entity ID Allocation` subsection)
- `crates/reddb-server/tests/runtime_query_behavior.rs` (new regression test)

## Notes for next iteration

- If/when the team decides to take option C (user-id space starts at 1),
  the test's exact-equality assertion is the right place to flip — bump
  the expected value and update both doc paragraphs in the same commit.
  The format-version bump and the migration plan are independent work.
- The 101-id reservation is implicit (it falls out of the descriptor-
  write order during bootstrap). If someone later adds another bootstrap
  record, the offset will move and this test will catch it before docs
  drift.
- Pre-existing failures in the same test binary
  (`config_reference_compares_stored_value_without_reparsing_sql`,
  `join_query_executes_against_real_table_rows`,
  `secret_reference_compares_vault_value_without_reparsing_sql`) are
  unrelated — surfaced from earlier slices.
