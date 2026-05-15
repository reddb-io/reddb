# PRD: MVCC read resolver for table-row visibility

GitHub: https://github.com/reddb-io/reddb/issues/508

Parent architecture PRD: [#507 Deepen correctness seams](https://github.com/reddb-io/reddb/issues/507),
repository copy: [deepen-correctness-seams.md](deepen-correctness-seams.md).

Follow-up implementation and conformance slices: [#509](https://github.com/reddb-io/reddb/issues/509),
[#510](https://github.com/reddb-io/reddb/issues/510), [#511](https://github.com/reddb-io/reddb/issues/511),
[#512](https://github.com/reddb-io/reddb/issues/512), [#513](https://github.com/reddb-io/reddb/issues/513),
and [#514](https://github.com/reddb-io/reddb/issues/514).

## Problem Statement

RedDB's table-row visibility rules are correctness-critical, but the read paths that need
those rules are not yet described as one shared internal seam. Table scans, indexed
candidates, logical-row lookup, DML target discovery, and historical `AS OF` table reads can
choose different candidate sources. They must still make the same final visibility decision
for a logical row at a given snapshot.

The parent correctness-seams PRD names the MVCC read resolver as the first storage
correctness tranche. This PRD materializes that tranche as a repository-local contract for
SQL table rows only. It is a planning and documentation artifact; it does not implement the
resolver.

The risk this PRD addresses is path-dependent visibility: one caller might honor tombstones,
another might use current rows directly, another might recheck indexes differently, and a
future `AS OF` path might accidentally become a separate history lookup policy. The resolver
exists so callers can vary how they find candidates without varying what "visible" means.

## Goals

- Define the MVCC read resolver as the shared visibility seam for SQL table rows.
- Keep the first slice aligned with [#507](https://github.com/reddb-io/reddb/issues/507) and
  [ADR 0014](../adr/0014-mvcc-history-store-and-transaction-recovery.md).
- Preserve current public behavior while tightening the internal ownership boundary.
- Cover table scan materialization, indexed candidate recheck, logical-row lookup, DML target
  scans, and `AS OF` table reads with one intended resolver contract.
- Reuse existing GitHub issues [#509](https://github.com/reddb-io/reddb/issues/509) through
  [#514](https://github.com/reddb-io/reddb/issues/514) as follow-up slices instead of creating
  duplicate implementation issues.

## Non-Goals

- This PRD does not implement the MVCC read resolver.
- This PRD does not change public SQL behavior.
- This PRD does not change public query syntax.
- This PRD does not change disk format.
- This PRD does not change WAL format.
- This PRD does not change RLS semantics.
- This PRD does not change authorization semantics.
- This PRD does not complete the full ADR 0014 history store.
- This PRD does not implement the full transaction write-set overlay.
- This PRD does not claim non-table multi-model visibility adoption.
- This PRD does not add historical secondary indexes, serializable isolation, SSI,
  distributed transaction atomicity, or autovacuum.

Compatibility expectation: no public SQL behavior, disk format, WAL format, public query syntax, RLS, or authorization change is assumed. Any later slice that needs one of those changes must say so in its own issue or ADR.

## Intended Resolver Interface

The resolver is an internal table-row visibility API. The intended conceptual interface is:

```text
resolve_visible_table_row(table, logical_row_id, snapshot, read_context) -> visible row | none
```

`read_context` may carry the current transaction scope, `AS OF` or VCS-pinned read state,
and any already-known candidate metadata. The interface is conceptual, not a required Rust
signature. Implementation slices may choose names and types that fit the storage modules.

The resolver owns these responsibilities for SQL table rows:

- Apply snapshot visibility for `xmin` and `xmax`.
- Treat legacy `xmin == 0` rows according to the existing compatibility rule.
- Apply tombstone visibility.
- Consult current-row state when the current version is enough to answer the read.
- Consult history state when an old snapshot, `AS OF`, or pinned read needs an older
  version.
- Apply committed and aborted xid decisions consistently.
- Overlay the current transaction's own writes when the implementing slice supports that
  part of the write set.
- Return no row when the logical row is not visible to the requested snapshot.
- Expose explicitly named physical inspection paths separately from user-data reads.

The resolver must not own authorization or RLS. Callers still perform authorization and RLS
through the existing statement and policy paths. The resolver decides storage visibility;
policy layers decide whether an otherwise visible row can be returned to the caller.

## Caller Migration Map

### Table scan materialization

Table scans may continue to enumerate current physical table storage as their candidate
source. Before a row is materialized into a user-visible result, the scan must resolve the
row's logical identity through the MVCC read resolver and return only the resolved visible
row. This is the [#509](https://github.com/reddb-io/reddb/issues/509) slice.

### Indexed candidate recheck

Secondary indexes may continue to serve current-index candidates first. Each indexed
candidate must be rechecked by logical row identity through the resolver before it is
returned. For old snapshots, `AS OF`, or pinned reads, the caller must fall back to a
version-aware scan or history lookup when the current index cannot prove completeness. This
is the [#510](https://github.com/reddb-io/reddb/issues/510) slice.

### Logical-row lookup

Direct lookup by a stable logical row identity must call the resolver instead of returning a
physical current entity directly. The lookup path can optimize candidate acquisition, but
the final row must come from the resolver's visibility decision. This is the
[#511](https://github.com/reddb-io/reddb/issues/511) slice.

### DML target scans

`UPDATE` and `DELETE` target discovery must use the same resolver decision as `SELECT`.
Mutation code should not find target rows through a looser current-row scan than the
read path would expose. This is the [#512](https://github.com/reddb-io/reddb/issues/512)
slice.

### AS OF table reads

Historical table reads must install or derive the requested snapshot first, then ask the
resolver for each candidate logical row. The `AS OF` caller owns parsing and snapshot
selection; the resolver owns which table-row version is visible at that snapshot. This is
the [#513](https://github.com/reddb-io/reddb/issues/513) slice.

## Implementation Decisions

- The first implementation scope is SQL table-row visibility only.
- The resolver is an internal seam, not a public API.
- Candidate discovery remains caller-specific; final visibility belongs to the resolver.
- Current secondary indexes remain acceptable candidate sources when followed by resolver
  recheck.
- Historical secondary indexes are deferred. Correct fallback is required before old
  snapshots can rely on current-index candidates.
- Authorization and RLS remain outside the resolver and keep their existing semantics.
- No public SQL behavior, disk format, WAL format, public query syntax, RLS, or
  authorization change is assumed by this PRD.
- Full history-store completion, full transaction write-set overlay, and non-table
  multi-model visibility are intentionally deferred to later slices.
- Implementation slices should use the existing domain vocabulary: `logical_id`,
  snapshot, tombstone, current store, history store, `AS OF`, and MVCC read resolver.

## Testing Decisions

- This PRD adds no engine tests because it is documentation-only.
- Each implementation follow-up should use public behavior tests rather than private
  resolver-shape tests.
- Table scan, indexed lookup, logical lookup, DML target scan, and `AS OF` tests should
  agree on the same visible rows for the same snapshot.
- Indexed-read coverage should include a case where the current index is not enough for an
  old snapshot and the caller must fall back.
- DML target coverage should prove `UPDATE` and `DELETE` do not mutate rows that the same
  snapshot could not read.
- Resolver conformance should live in the [#514](https://github.com/reddb-io/reddb/issues/514)
  slice after the first callers are migrated.

## Out of Scope

- Full ADR 0014 history-store completion.
- Full transaction write-set overlay.
- Non-table multi-model visibility adoption.
- New public SQL syntax or public behavior.
- Disk-format or WAL-format changes.
- RLS or authorization changes.
- Historical secondary indexes.
- Serializable isolation, SSI, two-phase commit, and distributed transactions.
- Autovacuum or background history compaction.

## Follow-Up Issue Map

| Issue | Role |
|:------|:-----|
| [#509](https://github.com/reddb-io/reddb/issues/509) Table scan uses MVCC read resolver | Route full table scan materialization through the resolver. |
| [#510](https://github.com/reddb-io/reddb/issues/510) Indexed table candidates recheck through MVCC read resolver | Recheck current-index candidates and require fallback when needed. |
| [#511](https://github.com/reddb-io/reddb/issues/511) Logical table-row lookup resolves through MVCC read resolver | Make direct logical row lookup use the shared visibility decision. |
| [#512](https://github.com/reddb-io/reddb/issues/512) DML target scans use MVCC read resolver | Align mutation target discovery with read visibility. |
| [#513](https://github.com/reddb-io/reddb/issues/513) AS OF table reads route through MVCC read resolver | Keep historical table reads on the same resolver seam. |
| [#514](https://github.com/reddb-io/reddb/issues/514) MVCC read resolver conformance pack and seam documentation | Pin the tranche with conformance tests and developer docs. |

These follow-up issues already exist. Do not create duplicates for this tranche.
