# GH-492: PRD - Rid and multi-model update surface

GitHub: https://github.com/reddb-io/reddb/issues/492

## Result

Published the Rid and multi-model update surface product plan into the repository:

- `docs/adr/0019-rid-and-multimodel-update-surface.md`
- `issues/prd/rid-and-multimodel-update-surface.md`
- `docs/_sidebar.md`

The ADR records `rid` as the canonical RedDB ID vocabulary, `item` as the public generic noun, the public item envelope, graph `from_rid` / `to_rid`, compound assignment scope, math function scope, multi-model update targets, testing expectations, and explicit out-of-scope items.

## Verification Notes

- `rtk git diff --check` passed.
- `rtk cargo fmt --all --check` passed.
- `rtk cargo test --test e2e_mvcc_read_resolver_conformance` passed.
- `rtk cargo check -p reddb-io-server` passed.
- `rtk make check` passed.
- `rtk cargo build --bin red` passed.
- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` failed on the existing repo-wide clippy backlog.
- `rtk pnpm typecheck` exited `1` while reporting `TypeScript: No errors found`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` reported 19 passed and 2 known failures:
  - `db helpers exist list and from round trip over stdio`: builder row count expected 1, got 0.
  - `embedded stdio ASK returns the full citation envelope (#406)`: cost default expected 0, got 0.000014.
