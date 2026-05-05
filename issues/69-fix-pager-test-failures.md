# Fix 3 pre-existing pager test failures [AFK]

GitHub: reddb-io/reddb#69
Pre-existing in WIP commit 599ad4b. Workspace split (#54) didn't introduce them.

Failing tests:
- `storage::engine::database::tests::test_database_open_create`
- `storage::engine::pager::tests::test_pager_create_new`
- `storage::engine::pager::tests::test_pager_reopen`

Symptom: `assertion left == right failed: left: 1, right: 3` at `crates/reddb-server/src/storage/engine/pager.rs:347` / `:364`.

## Acceptance Criteria
- [ ] Root cause identified in pager / database open path
- [ ] All 3 tests pass
- [ ] No regression in rest of `cargo test -p reddb-server`

## Feedback Loops
- `cargo test -p reddb-server --lib storage::engine::pager`
- `cargo test -p reddb-server --lib storage::engine::database`
