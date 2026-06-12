# RQL standard-SQL conformance corpus

Each `*.slt` file in this directory is a [sqllogictest][slt]-format script. The
harness (`tests/conformance.rs`) discovers every `.slt` file automatically and
runs it end-to-end against the current in-server engine
(`reddb-server`'s `RedDBRuntime::in_memory()`), one fresh runtime per file so
state never leaks between scripts.

## Truth is the SQLite oracle

The standard-SQL slice is sourced from the **public SQLite sqllogictest
corpus**. Every expected result block is the value **SQLite** produces — never
whatever the current engine happens to emit (ADR 0053). Engine output is the
thing under test, not the source of truth.

## Recording dialect divergences (skip, never drop)

When a standard-SQL case is genuinely correct against the SQLite oracle but the
RedDB engine diverges (a dialect difference, an unimplemented surface, or a
rendering seam), the case is **kept and skipped with a reason**, never silently
dropped. Use sqllogictest's conditional directive keyed on the engine name
(`reddb-server`) and write the reason in a comment directly above it:

```
# RedDB renders an integer SUM as a REAL; SQLite renders a bare integer.
# Divergence tracked under PRD #1098. Keep the oracle value; skip the run.
skipif reddb-server
query I nosort
SELECT SUM(v) FROM nums
----
80
```

`skipif reddb-server` skips that one record for our engine while leaving the
oracle-correct expectation on the page as documentation. `onlyif reddb-server`
is the inverse (run only on our engine) and should be avoided here — it would
let engine output masquerade as truth.

## Coverage map

Each slice is one `.slt` file holding a focused corner of standard SQL:

| File | Surface |
| --- | --- |
| `standard_select.slt` | projection, equality filter |
| `select_predicates.slt` | comparison / boolean / range / membership predicates |
| `select_expressions.slt` | integer arithmetic, `\|\|`, UPPER/LOWER/LENGTH/ABS, CASE |
| `select_arithmetic.slt` | `-` `*` operators, precedence, parentheses, unary minus |
| `select_string_functions.slt` | SUBSTR slicing, TRIM family, function composition |
| `select_like.slt` | LIKE `%` / `_` wildcards and the negated form |
| `select_case_searched.slt` | searched CASE, no-ELSE NULL fall-through, simple CASE, COALESCE |
| `select_null.slt` | IS NULL / IS NOT NULL, three-valued comparison, COALESCE |
| `select_distinct.slt` | SELECT DISTINCT over single and multiple columns |
| `select_order_limit.slt` | ORDER BY ASC/DESC, LIMIT, OFFSET |
| `select_order_multikey.slt` | multi-key ORDER BY with mixed directions |
| `select_join.slt` | INNER / LEFT join on an equi-key |
| `select_aggregate.slt` | COUNT/MIN/MAX, GROUP BY, HAVING |
| `select_aggregate_numeric.slt` | SUM / AVG numeric-aggregate rendering |
| `select_aggregate_count_variants.slt` | COUNT(\*) vs COUNT(col), COUNT(DISTINCT), MIN/MAX over text |

## Recorded divergences (skip-with-reason, never dropped)

Every case below is oracle-correct against SQLite but the RedDB engine diverges,
so it is kept on the page with `skipif reddb-server` and a reason inline. All are
tracked under PRD #1098; none are silently dropped (ADR 0053).

- **`NOT IN` / `NOT LIKE`** (`select_predicates.slt`, `select_like.slt`) — the
  parser rejects the infix `NOT` before `IN`/`LIKE`, though prefix
  `NOT <predicate>` and the bare membership forms both parse.
- **Simple `CASE <expr> WHEN`** (`select_case_searched.slt`) — only the searched
  `CASE WHEN <cond>` form parses; the selector form errors.
- **TRIM / LTRIM / RTRIM** (`select_string_functions.slt`) — catalogued but
  unimplemented in the SELECT path; they resolve to NULL.
- **SELECT-level DISTINCT** (`select_distinct.slt`) — DISTINCT is only accepted
  inside an aggregate argument, not as a projection quantifier.
- **Integer SUM renders as REAL** (`select_aggregate_numeric.slt`) — `SUM`
  surfaces a REAL, so `80` renders `80.000`.
- **Group-key-only GROUP BY** (`select_aggregate.slt`) — a projection of only the
  group key (no aggregate) does not collapse to one row per group and ignores
  HAVING.

## Cell rendering

The harness renders each engine `Value` into a comparison cell with the rules
SQLite's reference harness uses (see `src/conformance.rs`): `NULL` → the literal
`NULL`; text passes printable ASCII through and scrubs the rest to `@`; integers
print in decimal; reals print with exactly three decimals. The `query <types>`
header documents the intended column shape; the engine's intrinsic value kind
drives the actual rendering.

[slt]: https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki
