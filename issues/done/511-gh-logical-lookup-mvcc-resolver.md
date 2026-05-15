# [DONE] GH-511: Logical table-row lookup resolves through MVCC read resolver

GitHub issue: https://github.com/reddb-io/reddb/issues/511

## Objective

Route logical table-row lookup through the MVCC read resolver so stable row identity resolves
under the same visibility rules as scans.

## Scope

- Logical table-row lookup / stable row identity lookup paths only.
- Use the existing MVCC read resolver seam introduced for table scans and indexed candidates.
- Preserve current SQL, disk format, WAL format, and public behavior.
- Keep current-row fallback behavior explicit and behavior-preserving.
- Do not implement DML target scan routing (#512), AS OF table reads (#513), or the conformance pack (#514).

## Acceptance Criteria

- [x] Logical table-row lookup uses the MVCC read resolver for visibility and current-row selection.
- [x] Lookup by stable row identity agrees with table scan visibility for the same statement snapshot.
- [x] Current-row fallback behavior remains explicit and behavior-preserving.
- [x] Tests cover logical-row lookup parity with scan results.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Verification Notes

Passed:

- `rtk cargo test --test e2e_mvcc_logical_lookup historical_snapshot_logical_row_lookup_agrees_with_scan -- --nocapture`
- `rtk cargo test -p reddb-io-server runtime::table_row_mvcc_resolver`
- `rtk cargo test --test e2e_mvcc_logical_lookup`
- `rtk cargo test --test e2e_mvcc_index_recheck`
- `rtk cargo fmt --all --check`
- `rtk git diff --check`
- `rtk make check`
- `rtk cargo check -p reddb-io-server`
- `rtk cargo build --bin red`

Feedback loops:

- `rtk pnpm test` skips without `REDDB_BINARY_PATH` because the binary is built under `/home/cyber/.cache/cargo-target`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` still has the pre-existing builder row count and ASK cost default failures.
- `rtk pnpm typecheck` exits 1 while reporting `TypeScript: No errors found`.
