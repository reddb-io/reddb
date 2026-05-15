# [DONE] GH-509 Table scan uses MVCC read resolver

GitHub: https://github.com/reddb-io/reddb/issues/509

Parent PRD: https://github.com/reddb-io/reddb/issues/508

## What Changed

- Added `TableRowMvccReadResolver`, a narrow table-row-only MVCC read resolver.
- Routed universal, parallel, and sequential table scan materialization through the resolver for `TableRow` candidates.
- Preserved existing non-table entity visibility checks in the same scan helper.
- Added focused resolver unit coverage for live legacy rows, tombstoned fallback behavior, and captured snapshot `xmin` / `xmax` visibility.
- Added public query coverage proving a table scan inside an older snapshot still sees rows tombstoned by a later transaction.

## Acceptance Criteria

- [x] A table-row MVCC read resolver exists with a small interface for visibility checks under the current statement snapshot.
- [x] Table scan materialization uses the resolver instead of assembling visibility rules directly at the call site.
- [x] Existing table scan behavior is preserved for visible rows, tombstoned rows, and current-row fallback behavior.
- [x] Focused tests cover representative visible and invisible `xmin` / `xmax` combinations through the resolver.
- [x] Focused tests cover table scan results through public query behavior.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Verification

Passed:

- `rtk cargo fmt --all --check`
- `rtk git diff --check`
- `rtk cargo test -p reddb-io-server runtime::table_row_mvcc_resolver`
- `rtk cargo test -p reddb-io --test e2e_mvcc_delete_tombstones table_scan_preserves_snapshot_visibility_for_tombstoned_rows`
- `rtk make check`
- `rtk cargo check -p reddb-io-server`
- `rtk cargo build --bin red`

Known unrelated repo-level failures still present:

- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` fails with 92 existing warnings-as-errors outside this slice.
- `rtk pnpm typecheck` exits 1 while reporting `TypeScript: No errors found`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` reports 19 passed and 2 known failures:
  - `db helpers exist list and from round trip over stdio`
  - `embedded stdio ASK returns the full citation envelope (#406)`
