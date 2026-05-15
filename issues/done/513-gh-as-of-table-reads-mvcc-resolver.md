# GH-513: AS OF table reads route through MVCC read resolver

GitHub: https://github.com/reddb-io/reddb/issues/513

## Parent

Parent PRD: #508

## What to build

Route AS OF table reads through the MVCC read resolver without completing the full history-store implementation. Preserve the existing AS OF query surface and make historical-read behavior a normal resolver request rather than scattered special-case logic.

## Acceptance Criteria

- [x] AS OF table reads use the MVCC read resolver for table-row visibility decisions.
- [x] Existing AS OF table query syntax and behavior are preserved.
- [x] The resolver makes the current history-store absence or fallback behavior explicit.
- [x] Tests cover AS OF table reads through public query behavior.
- [x] Tests cover the resolver path used by AS OF reads where practical.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Blocked by

- #509
- #511

Both blockers are already closed on `main`.

## Scope Guard

- Do not implement the full history-store work.
- Do not change public SQL syntax, disk format, or WAL format.
- Keep `rid` as the canonical public identity vocabulary; `red_entity_id` / `entity_id` are legacy compatibility only.
- Keep this slice focused on AS OF table reads and the resolver path.

## Verification Notes

Record focused test commands and known unrelated failures before moving this issue to `issues/done/`.

- `rtk cargo fmt --all --check` passes.
- `rtk git diff --check` passes.
- `rtk cargo test --test e2e_vcs_as_of_enforce as_of_commit_table_scan_reads_snapshot_visible_row_version -- --nocapture` passes.
- `rtk cargo test --test e2e_vcs_as_of_enforce` passes.
- `rtk cargo test -p reddb-io-server table_row_mvcc_resolver -- --nocapture` passes.
- `rtk cargo test --test e2e_vcs_opt_in` passes.
- `rtk cargo test --test e2e_mvcc_logical_lookup` passes.
- `rtk cargo test --test e2e_mvcc_index_recheck` passes.
- `rtk cargo check -p reddb-io-server` passes.
- `rtk make check` passes.
- `rtk cargo build --bin red` passes.
- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` still fails on the pre-existing 92 warnings-as-errors backlog.
- `rtk pnpm typecheck` still exits 1 while reporting `TypeScript: No errors found`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` still has the pre-existing builder row count and ASK cost default failures.
