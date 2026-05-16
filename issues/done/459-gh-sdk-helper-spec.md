# GH-459: SDK Helper Spec

GitHub: https://github.com/reddb-io/reddb/issues/459

## Result

Published a versioned SDK Helper Spec:

- `docs/clients/sdk-helper-spec.md`
- `docs/_sidebar.md`

The spec defines canonical helper names, input and output envelopes, error
codes, HTTP JSON as the semantic baseline, rich helper coverage for generic
query/insert/bulkInsert, documents, KV, queues, transactions, and probabilistic
structures, plus cross-driver conformance case names and README expectations.

## Verification Notes

- `rtk git diff --check` passed.
- `rtk cargo fmt --all --check` passed.
- `rtk cargo check -p reddb-io-server` passed.
- `rtk make check` passed.
- `rtk cargo build --bin red` passed.
- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` failed on the existing repo-wide clippy backlog.
- `rtk pnpm typecheck` exited `1` while reporting `TypeScript: No errors found`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` reported 19 passed and 2 known failures:
  - `db helpers exist list and from round trip over stdio`: builder row count expected 1, got 0.
  - `embedded stdio ASK returns the full citation envelope (#406)`: cost default expected 0, got 0.000014.
