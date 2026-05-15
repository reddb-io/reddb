# GH-512: DML target scans use MVCC read resolver

GitHub issue: https://github.com/reddb-io/reddb/issues/512

## Objective

Route table-row `UPDATE` and `DELETE` target selection through the MVCC read resolver so DML
affects the same visible row set that a matching `SELECT` would observe under the statement
snapshot.

## Scope

- Table-row DML target selection for `UPDATE` and `DELETE` only.
- Use the existing `TableRowMvccReadResolver` seam from #509-#511.
- Preserve current write behavior, RLS checks, authorization checks, SQL surface, disk format, and WAL format.
- Do not implement AS OF reads (#513) or the conformance pack/docs (#514).

## Acceptance Criteria

- [x] Table-row DML target scans use the MVCC read resolver for candidate visibility.
- [x] `UPDATE` affects only rows visible through the resolver for the statement snapshot.
- [x] `DELETE` affects only rows visible through the resolver for the statement snapshot.
- [x] Tests prove parity between a matching `SELECT` result set and rows affected by `UPDATE` / `DELETE`.
- [x] Existing RLS and authorization behavior is preserved.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Verification Notes

Record focused test commands and known unrelated failures before moving this issue to `issues/done/`.

- `rtk cargo fmt --all --check` passes.
- `rtk git diff --check` passes.
- `rtk cargo test --test e2e_mvcc_dml_target_scans -- --nocapture` passes.
- `rtk cargo test --test e2e_mvcc_logical_lookup -- --nocapture` passes.
- `rtk cargo test -p reddb-io-server dml_target_scan -- --nocapture` passes.
- `rtk cargo test -p reddb-io-server table_row_mvcc_resolver -- --nocapture` passes.
- `rtk make check` passes.
- `rtk cargo check -p reddb-io-server` passes.
- `rtk cargo build --bin red` passes.
- `rtk pnpm test` skips without `REDDB_BINARY_PATH` because `target/debug/red` is absent.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` still has the pre-existing builder row count and ASK cost default failures.
- `rtk pnpm typecheck` exits 1 while reporting `TypeScript: No errors found`.
