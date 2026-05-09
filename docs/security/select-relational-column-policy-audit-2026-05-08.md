# SELECT Relational Column Policy Audit

Issue: #265 Wire SELECT relational
Date: 2026-05-08
Status: historical prep audit; superseded by
[Column Enforcement Coverage](column-enforcement-coverage.md) after #265-#269.

> This note is retained as the pre-implementation path audit for relational
> `SELECT`. The final runtime state covers explicit projections, `SELECT *`,
> and joins. Use [Column Enforcement Coverage](column-enforcement-coverage.md)
> for the current matrix.

## Scope

This note maps the relational `SELECT` execution path and the exact places where
column policy enforcement should be inserted once #264 provides the final
`ColumnPolicyGate` contract. It intentionally does not implement policy
decisions.

Relational `SELECT` means `QueryExpr::Table` and `QueryExpr::Join`, including
view-rewritten, CTE-inlined, subquery-in-`FROM`, and prepared/direct-AST
execution. Vector, hybrid, graph/path, queue, and virtual `red.*` schema paths
are out of scope except where they can appear as a join side.

## Entry Points

1. SQL text enters `RedDBRuntime::execute_query_inner` in
   `crates/reddb-server/src/runtime/impl_core.rs`.
2. `WITHIN` scope is stripped before statement-frame construction.
3. `StatementExecutionFrame::build` installs config, secret, auth, tenant,
   snapshot, and cache context.
4. `WITH` queries are parsed and inlined, then re-enter through
   `execute_query_expr`.
5. Normal SQL parse/cache resolves to `QueryExpr`, view names are rewritten,
   `frame.check_query_privilege` runs, intent locks are acquired, and the
   executor dispatches:
   - `QueryExpr::Table` around lines 4107-4202.
   - `QueryExpr::Join` around lines 4204-4239.
6. Prepared/direct-AST callers use `execute_query_expr`, then `dispatch_expr`
   around lines 5329-5502.

## Current Authorization Shape

The SQL-text path performs a coarse privilege check before relational execution.
For table `SELECT`, RLS is folded into the table filter before calling
`execute_runtime_table_query`. For joins, RLS is folded into each table leaf
before calling `execute_runtime_join_query`.

The prepared/direct-AST path performs `check_query_privilege`, then dispatches
table and join queries directly. It does not currently mirror the SQL-text
path's RLS folding in `dispatch_expr`; #265 column policy wiring should either
share a single relational authorization prelude with the SQL-text path or make
the prepared path call that same helper. Otherwise prepared relational SELECTs
will be an easy bypass for column policy enforcement.

## Table SELECT Execution

Top-level table dispatch calls:

```text
execute_runtime_table_query(db, table_with_rls, Some(index_store))
```

`execute_runtime_table_query` delegates to
`execute_runtime_canonical_table_query_indexed` in
`crates/reddb-server/src/runtime/query_exec/table.rs`.

Important materialization paths:

- FROM subquery: lines 63-116 recursively execute the inner `TableQuery`, then
  apply the outer filter/sort/offset/limit over materialized records.
- Direct `_entity_id` fast path: lines 128-140 materialize the whole row with
  `runtime_table_record_from_entity`.
- Sorted-index covered path: lines 180-213 can return projected column values
  directly from index data, without fetching entities.
- Sorted-index heap path: lines 257-326 materializes either lean `SELECT *` rows
  or explicit projected rows from fetched entities.
- Cross-index covered path: lines 384-444 can synthesize projected rows from
  equality predicates, without heap access.
- Cross-index heap path: lines 447-479 materializes lean or explicit rows.
- Multi-hash bitmap covered path: lines 557-585 can synthesize projected rows
  from equality predicates, without heap access.
- Multi-hash bitmap heap path: lines 618-639 materializes lean or explicit
  rows.
- Hash index-only path: lines 647-703 can synthesize projected rows from an
  equality predicate, without heap access.
- Hash heap path: lines 776-810 materializes lean or explicit rows.
- Filtered scan fast path: lines 817-1079 materializes only rows that pass the
  compiled filter, with separate sequential and parallel branches.
- Unfiltered scan fast path: lines 1082-1138 calls
  `scan_runtime_table_source_records_limited`, then sorts/offsets/limits.
- Canonical planner path: lines 1141-1443 walks logical nodes and applies
  projection at the `"projection" | "document_projection" | "entity_projection"`
  node.

Projection helpers are split:

- Explicit-column materialization uses record-search helpers such as
  `runtime_table_record_from_entity_projected`,
  `runtime_table_record_from_entity_ref_projected`, and
  `runtime_table_record_with_col_indices`.
- Final projection nodes use `project_runtime_record_with_db` in
  `crates/reddb-server/src/runtime/join_filter.rs` lines 574-660.
- `SELECT *` column reporting uses `projected_columns` /
  `collect_visible_columns`, not only materialized row keys.

## Join SELECT Execution

Top-level join dispatch calls:

```text
execute_runtime_join_query(db, join_with_rls)
```

`execute_runtime_join_query` in
`crates/reddb-server/src/runtime/query_exec/join.rs` builds a canonical plan,
executes it, then computes output columns from effective projections.

Important join points:

- Join plan execution starts around lines 32-38.
- The projection logical node applies `project_runtime_join_record_with_db`
  around lines 99-116.
- Join base execution gets left and right records around lines 152-165 by
  calling `execute_runtime_canonical_expr_node` for each side.
- Join output projection is implemented in `project_runtime_join_record_with_db`
  around lines 380-448.
- Qualified field resolution is handled by `resolve_runtime_join_field` around
  lines 330-358.

Column policy enforcement for joins must be side-aware. A projected field like
`users.email` should check the `users.email` resource, while unqualified
`email` is ambiguous when both sides expose it. The gate should either resolve
unqualified fields against the same table-context rules used by
`resolve_runtime_join_field`, or deny ambiguous unqualified columns before
projection.

## Required Insertion Points After #264

1. Add a shared relational authorization prelude in `impl_core.rs` before both
   SQL-text and prepared/direct-AST relational dispatch.
   - Input: effective scope/read frame, rewritten `QueryExpr`.
   - Output: authorized/restricted relational `QueryExpr` plus any projection
     metadata the executor needs.
   - Must be called by both `execute_query_inner` and `dispatch_expr`.

2. Check requested projection columns before execution.
   - For `TableQuery`, inspect `effective_table_projections(query)`.
   - For `JoinQuery`, inspect `effective_join_projections(query)`.
   - `SELECT *` must expand against the source schema before execution or be
     represented as an allowlist/denylist passed down to materializers. A
     post-result filter is too late for index-only and covered paths that
     synthesize rows without heap access.

3. Thread column policy context into table execution.
   - `execute_runtime_table_query`.
   - `execute_runtime_canonical_table_query_indexed`.
   - `RuntimeTableExecutionContext`.
   - Every covered/index-only return path that currently constructs records
     directly.
   - Every explicit-column/lean/full-row materializer call.
   - Final projection through `project_runtime_record_with_db`.

4. Thread column policy context into join execution.
   - `execute_runtime_join_query`.
   - `execute_runtime_canonical_join_query`.
   - `execute_runtime_canonical_join_node`.
   - `project_runtime_join_record_with_db`.
   - Join side scans via `execute_runtime_canonical_expr_node`.

5. Filter result column metadata through the same decision.
   - `projected_columns` for table and join results.
   - `collect_visible_columns` for `SELECT *`.
   - Any pre-serialized or wire-schema path that derives columns from records.

## Blockers For #265 Implementation

- #264 must define the final `ColumnPolicyGate` API: decision type, resource
  naming, deny behavior, audit behavior, and how to represent `SELECT *`.
- The gate must expose enough context for prepared/direct-AST execution, not
  only SQL text execution.
- Join ambiguity semantics need to be settled for unqualified duplicate column
  names.
- Covered/index-only scans need a policy decision before they synthesize records;
  a projection-only filter cannot protect those paths.
- Subquery and view semantics need one rule: enforce on underlying base columns,
  exposed aliases, or both. The current FROM-subquery path materializes inner
  projection aliases before the outer query runs.

## Low-Risk Prep Outcome

No code scaffolding was added in this prep pass. Without #264's final API,
introducing placeholder types would create churn across hot table-scan and join
paths. This audit note is the useful independent artifact for #265.
