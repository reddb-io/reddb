# GH-467: PRD - tiered file layout

GitHub: https://github.com/reddb-io/reddb/issues/467

## Result

Published a durable local PRD artifact for the tiered file layout plan:

- `issues/prd/tiered-file-layout.md`

The PRD links to ADR 0018, distinguishes files-in-directory layout from ADR
0003's bytes-on-disk contract, captures the four layout tiers, records the
phased delivery plan, and keeps this slice documentation-only.

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
