# `storage/query/planner` — Cost-based query planner

The planner takes a parsed `QueryExpr` and produces an executable
`CanonicalLogicalPlan`, choosing access paths and join orders by consulting
real per-table / per-column statistics through a pluggable `StatsProvider`.

## Module layout

- `cost.rs` — `CostEstimator`, `PlanCost`, `CardinalityEstimate`,
  `TableStats`, `ColumnStats`, `filter_selectivity` (the recursive
  selectivity walker)
- `stats_provider.rs` — `StatsProvider` trait + `NullProvider`,
  `StaticProvider`, `RegistryProvider`
- `cache.rs` — `PlanCache` LRU for compiled plans
- `logical.rs` / `logical_helpers.rs` — `CanonicalLogicalPlan` build
- `optimizer.rs` — pass-based plan rewriter
- `rewriter.rs` — AST-level rewrites
- `types.rs` — public re-exports

After Target 5 (`PLAN.md`):
- `histogram.rs` — `Histogram`, `Bucket`, `MostCommonValues`,
  `equi_depth_from_sample`

## Invariants

### 1. Cost is `cpu + io*10 + memory*0.1`; after Target 2, `startup_cost` is the LIMIT tiebreaker

`PlanCost::new(cpu, io, memory)` (`cost.rs:74-85`) computes
`total = cpu + io*10 + memory*0.1`. IO is 10× more expensive than CPU
because we expect cold-cache reads to dominate any non-trivial scan.

After Target 2, `PlanCost` gains a `startup_cost: f64` field. Plan
selection uses:

- `total` as the default tiebreaker.
- `startup_cost` when a `LIMIT k` is in scope and `k < 0.1 * cardinality`
  (i.e. "client wants top-k fast, even if total work is higher").

The startup-vs-total split must be respected by every operator that
estimates cost. Sequential scans report `startup = 0`. Sorts and hash
joins (build side) report `startup = total` because they cannot produce
the first row without consuming all input.

### 2. `StatsProvider` is the only source of real numbers; `NullProvider` must never panic

`CostEstimator::with_stats(provider)` (`cost.rs`) injects a `StatsProvider`
trait object. The default is `NullProvider` (`stats_provider.rs`), which
returns `None` for everything and falls back to the heuristic constants in
`filter_selectivity`.

**`NullProvider` must never panic** — the planner must always be able to
build a plan, even in cold-start situations where no stats have been
gathered. Every method on `StatsProvider` defaults to `None` for exactly
this reason.

When you add a new statistic (Target 5: `column_histogram`,
`column_mcv`), follow the same rule: extend the trait with a `default
None` method, and use `if let Some(...)` in `filter_selectivity` with a
graceful fallback.

### 3. `filter_selectivity` recurses structurally; leaves consult the provider

`CostEstimator::filter_selectivity(filter, table)` (`cost.rs:302-361`) is
a recursive walk over the AST `Filter` enum. The shape:

```
Compare { field, op, .. }            → consult provider, fall back to op-default
Between/In/Like/IsNull/IsNotNull     → consult provider where applicable
And(left, right)                     → s(left) * s(right)
Or(left, right)                      → s(left) + s(right) - s(left)*s(right)
Not(inner)                           → 1.0 - s(inner)
```

The composition rules (multiplication for AND, inclusion-exclusion for OR)
assume **independence** of the leaves. This is the standard textbook
assumption and is wrong on correlated columns — postgres has the same
limitation and addresses it with `pg_statistic_ext`. We do not have
extended stats yet; document over-estimates as a known limitation.

### 4. Heuristic constants are the floor, not the ceiling

The `filter_selectivity` fallbacks (`cost.rs:302-361`) are:

| Operator | Heuristic |
|---|---|
| `Eq` | 0.01 |
| `Ne` | 0.99 |
| `Lt`/`Le`/`Gt`/`Ge` | 0.3 (capped) |
| `Between` | 0.25 |
| `In` | `len * 0.01` capped at 0.5 |
| `Like` / `Contains` | 0.10 |
| `StartsWith` / `EndsWith` | 0.15 |
| `IsNull` | 0.01 |
| `IsNotNull` | 0.99 |

These exist for the cold-start path. **Never raise them as a fix for a
bad plan.** If the planner is wrong on a real workload, the right answer is
to gather stats (histogram, MCV, sample) and let the provider override the
heuristic — not to retune the constant.

After Target 5, histogram bucket arithmetic supersedes the 0.3 cap on
range predicates and MCV lookups supersede the 0.01 fallback on equality.
The heuristics still trigger when the provider returns `None`.

### 5. The plan cache is keyed by AST shape, not by literal values

`PlanCache` (`cache.rs`) memoizes compiled plans. The cache key is the
**shape** of the AST after parameter binding — two queries with different
literal constants but the same shape share a cached plan.

This is what lets repeated point lookups skip the planner entirely. **Do
not key by the full AST string** — that defeats the cache for the most
common case (parameterised queries from drivers).

If you change the cache key strategy, also update `PlanCache::invalidate`
to reflect the new equivalence class.

## Anti-patterns to avoid

- **Calling `StatsProvider` from inside the executor** — stats are for the
  planner. The executor sees the chosen plan and runs it; if it had to
  re-estimate, the cost model would loop back on itself.
- **Hardcoding selectivity in `optimizer.rs`** — selectivity belongs in
  `cost.rs::filter_selectivity`. The optimizer consumes the cost.
- **Using `f64::INFINITY` as a "skip this plan" sentinel** — use
  `Option<PlanCost>` and propagate `None`.

## See also

- Stats provider impls: `src/storage/query/planner/stats_provider.rs`
- Index trait the registry exposes: `src/storage/index/README.md`
- Plan cache strategy: `src/storage/query/planner/cache.rs`
- Future direction: `PLAN.md` § Targets 2 and 5
