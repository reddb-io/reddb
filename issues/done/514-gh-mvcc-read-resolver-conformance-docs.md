# GH-514: MVCC read resolver conformance pack and seam documentation

GitHub: https://github.com/reddb-io/reddb/issues/514

## Parent

Parent PRD: #508

## What to build

Add a conformance pack and short seam documentation for the MVCC read resolver after the initial table scan, index, logical lookup, DML target, and AS OF call sites have moved behind it. Prove the resolver is now the intended table-row visibility Interface and document how future table read paths should use it.

## Acceptance Criteria

- [x] Conformance tests compare table scan, indexed read, logical-row lookup, DML target selection, and AS OF behavior through the resolver-backed paths.
- [x] Tests verify indexed and non-indexed queries return the same visible row set for equivalent predicates.
- [x] Tests verify matching `SELECT`, `UPDATE`, and `DELETE` visibility behavior where applicable.
- [x] Short documentation identifies the MVCC read resolver as the table-row visibility seam.
- [x] Documentation states what is intentionally out of scope for this slice: full history store, new WAL/disk format, and full transaction write-set overlay.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Blocked by

- #509
- #510
- #511
- #512
- #513

All blockers are already closed on `main`.

## Scope Guard

- Conformance and documentation only unless a tested conformance gap is found.
- Do not implement the full history store.
- Do not change public SQL syntax, disk format, or WAL format.
- Keep `rid` as canonical public identity vocabulary; `red_entity_id` / `entity_id` are legacy compatibility only.

## Verification Notes

Record focused test commands and known unrelated failures before moving this issue to `issues/done/`.

- `rtk cargo test --test e2e_mvcc_read_resolver_conformance -- --nocapture` passes.
- `rtk cargo test --test e2e_mvcc_logical_lookup` passes.
- `rtk cargo test --test e2e_mvcc_dml_target_scans` passes.
- `rtk cargo test --test e2e_vcs_as_of_enforce` passes.
- `rtk cargo test -p reddb-io-server table_row_mvcc_resolver` passes.
- `rtk cargo fmt --all --check` passes.
- `rtk git diff --check` passes.
- `rtk cargo check -p reddb-io-server` passes.
- `rtk make check` passes.
- `rtk cargo build --bin red` passes.
- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` still fails on the pre-existing 92 warnings-as-errors backlog.
- `rtk pnpm typecheck` still exits 1 while reporting `TypeScript: No errors found`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` still has the pre-existing builder row count and ASK cost default failures.
