# [AFK] GH-507 PRD: Deepen RedDB correctness seams for MVCC, events, queues, and catalog

GitHub: https://github.com/reddb-io/reddb/issues/507

## Goal

Materialize the GitHub PRD for deepening RedDB correctness seams in the repository and make the GitHub issue closable with traceable follow-up slices.

This is a PRD/documentation issue, not the place to implement MVCC, event, queue, statement-frame, catalog, or wire-adapter code.

## Problem summary

RedDB has correctness-critical behavior implemented through shallow or partially bypassed seams:

- MVCC read resolution leaks snapshot visibility, tombstones, AS OF behavior, current-row fallback, write-set overlay, and index fallback into callers.
- Transaction commit behavior is spread across journals, pending tombstones, pending versioned updates, deferred store WAL capture, conflict checks, rollback revival, and snapshot publication.
- `WITH EVENTS` atomicity depends on DML wrappers, WAL capture, event shaping, filtering, redaction, tenant queue naming, queue backpressure, and DLQ routing lining up.
- Queue delivery and retirement leak dispatch, config/meta persistence, lifecycle decisions, DLQ replay, and result shaping.
- Statement frame is intended to be the single query lifecycle seam but fast paths, prepared/direct paths, and wire adapters can bypass it.
- Catalog discovery is split between `CollectionDescriptor`, `red.*`, `SHOW`, docs, and wire-specific catalog handling.
- Postgres wire catalog handling partially materializes catalog views instead of consistently translating native RedDB catalog concepts.

## Expected repository artifact

Create or update a PRD document under the repo docs, using existing project conventions. Prefer a path like:

- `docs/prd/deepen-correctness-seams.md`

The PRD should preserve the GitHub issue content in repository form, but organize it for implementation:

- Problem statement.
- Goals and non-goals.
- Deep modules and their intended interfaces:
  - MVCC read resolver.
  - Transaction commit unit.
  - Event-enabled collection emission.
  - Queue lifecycle.
  - Statement frame lifecycle.
  - Catalog discovery.
  - Wire catalog translation.
- User stories or maintainer outcomes.
- Implementation decisions.
- Testing decisions.
- Out-of-scope section.
- Tracer-bullet issue map.

## Follow-up issue map

Confirmed with `gh issue view` on 2026-05-15 that these existing GitHub follow-ups cover the first MVCC tranche:

- [#508](https://github.com/reddb-io/reddb/issues/508) PRD: MVCC read resolver for table-row visibility.
- [#509](https://github.com/reddb-io/reddb/issues/509) Table scan uses MVCC read resolver.
- [#510](https://github.com/reddb-io/reddb/issues/510) Indexed table candidates recheck through MVCC read resolver.
- [#511](https://github.com/reddb-io/reddb/issues/511) Logical table-row lookup resolves through MVCC read resolver.
- [#512](https://github.com/reddb-io/reddb/issues/512) DML target scans use MVCC read resolver.
- [#513](https://github.com/reddb-io/reddb/issues/513) AS OF table reads route through MVCC read resolver.
- [#514](https://github.com/reddb-io/reddb/issues/514) MVCC read resolver conformance pack and seam documentation.

No duplicate issues were created. Non-MVCC gaps are documented as future split candidates in the PRD.

## Acceptance criteria

- [x] Repository contains a PRD document for the correctness-seam architecture hardening program.
- [x] PRD uses RedDB domain vocabulary consistently: MVCC read resolver, Transaction commit unit, Event-enabled collection, Queue delivery, Pending delivery, Queue retirement, Statement frame, CollectionDescriptor, Catalog discovery, Wire adapter, EffectiveScope, and OperatorEvent.
- [x] PRD states public behavior compatibility expectations and explicitly says no public syntax, schema, disk-format, or wire-protocol change is assumed.
- [x] PRD names the first implementation tranche and links #508-#514 as the MVCC read-resolver slice.
- [x] PRD keeps non-MVCC areas as future split candidates rather than pretending they are implemented.
- [x] Existing docs index or PRD index is updated if the repo has one.
- [x] Local issue file is moved to `issues/done/` with completed checkboxes.
- [x] Verification commands are run and recorded.

## Verification

- `rtk bash -lc 'set -euo pipefail ...'` PRD contract check: passed.
- `rtk git diff --check`: passed.
- `rtk python3 scripts/check-sql-reference.py`: passed.
- `rtk cargo fmt --all --check`: passed.
- `rtk cargo check -p reddb-io-server`: passed.
- `rtk cargo build --bin red`: passed.
- `rtk proxy pnpm test`: exited 0 but skipped because `target/debug/red` is absent when Cargo uses `/home/cyber/.cache/cargo-target`.
- `rtk proxy env REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red pnpm test`: failed with two pre-existing smoke failures: builder row count expected 1 got 0, and ASK cost default expected 0 got 0.000014.
- `rtk proxy pnpm typecheck`: failed because the root package has no `typecheck` command.
