# [AFK] GH-510 Indexed table candidates recheck through MVCC read resolver

GitHub: https://github.com/reddb-io/reddb/issues/510

Parent PRD: https://github.com/reddb-io/reddb/issues/508

Blocked by: #509, now completed in `main`.

## What To Build

Route indexed table candidates through the table-row MVCC read resolver before they are
materialized as query results. Preserve existing indexed query behavior while proving that indexed
and non-indexed reads agree on table-row visibility.

This slice must stay narrow:

- SQL table-row indexed read paths only.
- Candidate recheck before result materialization only.
- Preserve public SQL behavior.
- Preserve disk format and WAL format.
- Do not implement logical-row lookup, DML target scans, or AS OF routing here; those are #511-#513.

## Acceptance Criteria

- [x] Indexed table read paths recheck candidate rows through the MVCC read resolver.
- [x] Invisible or tombstoned stale index candidates are rejected by the resolver before result materialization.
- [x] Indexed and non-indexed queries return the same visible row set for equivalent predicates.
- [x] Tests cover at least one stale or invisible indexed candidate scenario.
- [x] No public SQL, disk-format, or WAL-format behavior changes are introduced.
- [x] `make check` and relevant focused Rust tests pass.

## Implementation Notes

- Reuse the #509 `TableRowMvccReadResolver`; do not duplicate visibility rules.
- Prefer the smallest call-site change in existing indexed scan/query code.
- If a current indexed path already falls back to heap scan under snapshots, preserve that behavior and add a test proving equivalent visible rows.
- Keep authorization, RLS, point lookup, DML target scans, and AS OF handling outside this slice unless needed to preserve indexed read behavior.

## Verification

Run:

- focused indexed MVCC query tests
- relevant resolver/table scan tests if touched
- `rtk make check`
- `rtk cargo fmt --all --check`
- `rtk cargo check -p reddb-io-server`
- `rtk cargo build --bin red`
- repo lint/typecheck/test gates required by the outer workflow, recording known unrelated failures instead of changing unrelated code
