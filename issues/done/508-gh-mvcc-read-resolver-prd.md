# [AFK] GH-508 PRD: MVCC read resolver for table-row visibility

GitHub: https://github.com/reddb-io/reddb/issues/508

## Goal

Materialize the GitHub PRD for the MVCC read resolver as a repository artifact and make
the issue closable with traceable follow-up slices.

This is a PRD/documentation issue. Do not implement the resolver in this issue.

## Scope

Create or update a PRD document under `docs/prd/` for table-row MVCC visibility.

The PRD must preserve the GitHub issue decisions:

- Child of #507.
- First storage-correctness slice from the parent correctness-seams PRD.
- Initial scope is table-row visibility only.
- Preserve public SQL behavior, disk format, WAL format, public query syntax, RLS, and authorization semantics.
- Define the MVCC read resolver as the shared seam for table scans, DML target scans, logical-row lookup, indexed candidate recheck, and AS OF table reads.
- Keep full ADR 0014 history-store completion, full transaction write-set overlay, and non-table multi-model visibility out of scope.

## Required PRD Content

- Problem statement.
- Goals and non-goals.
- Intended resolver interface and responsibilities.
- Caller migration map:
  - table scan materialization,
  - indexed candidate recheck,
  - logical-row lookup,
  - DML target scans,
  - AS OF table reads.
- Implementation decisions.
- Testing decisions.
- Out-of-scope section.
- Follow-up issue map, reusing existing #509-#514 where applicable instead of creating duplicates.

## Acceptance Criteria

- [x] Repository contains a PRD document for the MVCC read resolver table-row visibility slice.
- [x] PRD links #507 as parent architecture PRD.
- [x] PRD links #509-#514 as implementation/conformance follow-ups.
- [x] PRD explicitly says no public SQL behavior, disk format, WAL format, public query syntax, RLS, or authorization change is assumed.
- [x] PRD keeps full history-store completion, full write-set overlay, and non-table multi-model visibility out of scope.
- [x] Existing docs index/sidebar is updated if needed.
- [x] Local issue file is moved to `issues/done/` with completed checkboxes.
- [x] Verification commands are run and recorded.

## Completed

- Added `docs/prd/mvcc-read-resolver-table-row-visibility.md`.
- Linked the new PRD from `docs/_sidebar.md`.
- Kept the artifact documentation-only; no resolver implementation was added.
- Reused existing GitHub follow-ups #509-#514 instead of creating duplicates.

## Verification

Run on 2026-05-15:

- `rtk git diff --check` passed.
- PRD contract check for required terms and issue links passed: 25 required terms present.
- `rtk python3 scripts/check-sql-reference.py` passed.
- `rtk cargo fmt --all --check` passed.
- `rtk cargo check -p reddb-io-server` passed.
- `rtk cargo build --bin red` passed.
- `rtk pnpm test` skipped because the script looks for `target/debug/red`; this worktree uses `/home/cyber/.cache/cargo-target/debug/red`.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` failed with the two pre-existing JS smoke failures noted by prior work:
  - `db helpers exist list and from round trip over stdio`: builder row count expected 1, got 0.
  - `embedded stdio ASK returns the full citation envelope (#406)`: cost default expected 0, got 0.000014.
- `rtk pnpm typecheck` failed because the root package has no `typecheck` script; it falls through to a `tsc` shim. `package.json` scripts contain `postinstall`, `test`, `changeset`, `release:version`, `release:publish`, and `version`.
