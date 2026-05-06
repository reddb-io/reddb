# When NOT to use RedDB (yet)

Status: 2026-05-06 — gaps from the canonical `duel-official` lock
(issue #154). Companion to [`wins.md`](wins.md).

This page is the honest counterpart to "where RedDB wins". Each gap
below is reproducible from the same canonical bench, cites a session
id, and links to the in-flight issue that closes it. If you are
evaluating RedDB for a workload that lives in this list, either wait
for the closure issue or pick the engine that already wins.

## Methodology

Same as `wins.md`: numbers come from the locked `BenchConfigSchema`
(`make duel-official`, `profile=standard`, 10 runs). The rolling
aggregate from these sessions is published in
`rdb-benchmark/BASELINE.md`. We cite a single representative session
per scenario for traceability.

For a faster local check, use the dev variant `make mini-duel`.

## Gap 1 — `concurrent`: ≈ 49× behind MongoDB

**What this measures.** Mixed insert/update throughput under 16
concurrent worker connections. Stresses commit-path serialisation,
not single-thread CPU.

**Cited session.** `sess-20260416215030-1190998`
(`rdb-benchmark/benchmarks/history.jsonl`, mongo lock window).
`BASELINE.md` aggregate: RedDB 241 ops/s vs Mongo 11,844 ops/s.

**Closure issue.** [#157 — WAL lock-free append coordinator](../../issues/done/157-wal-lock-free-segqueue.md)
(now shipped on `main`; re-bench expected to compress this gap
toward 5–10× of Mongo, full closure tracked in
[`docs/perf/roadmap.md`](roadmap.md) item 2).

**Reproducing.**

```bash
cd rdb-benchmark
make duel-official OFFICIAL_SCENARIOS=concurrent OFFICIAL_DB=duel-core
```

## Gap 2 — `bulk_update`: ≈ 30× behind PostgreSQL

**What this measures.** A bulk `UPDATE` over a populated collection
that hits the same set of B-tree leaves repeatedly. PostgreSQL's HOT
update path is the apex predator here; RedDB currently re-walks the
B-tree per row.

**Cited session.** `sess-20260416214507-1181555` (canonical lock
window). `BASELINE.md` aggregate: RedDB 1,303 ops/s vs PG 39,171
ops/s, Mongo 5,803 ops/s.

**Closure issue.** [#159 — BTree batch upsert by leaf](../../issues/159-btree-batch-upsert-by-leaf.md)
(in flight). Groups updates by target leaf so each leaf is opened,
mutated, and persisted exactly once per batch.

**Reproducing.**

```bash
cd rdb-benchmark
make duel-official OFFICIAL_SCENARIOS=bulk_update OFFICIAL_DB=duel-core
```

## Gap 3 — `aggregate_group`: ≈ 12× behind PostgreSQL

**What this measures.** `SELECT ... GROUP BY ...` over a populated
collection. PostgreSQL plans this through a hash aggregate; RedDB
currently materialises rows into `HashMap<String, Value>` per group
and pays for each rehydration.

**Cited session.** `sess-20260416214507-1181555` (canonical lock
window). `BASELINE.md` aggregate: RedDB 9 ops/s vs PG 105 ops/s
(Mongo n/a).

**Closure issue.** [#161 — AggregateQueryPlanner](../../issues/161-aggregate-query-planner.md)
(in flight). Replaces the per-row HashMap rehydration with a
columnar group-by over the underlying page representation.

**Reproducing.**

```bash
cd rdb-benchmark
make duel-official OFFICIAL_SCENARIOS=aggregate_group OFFICIAL_DB=duel-core
```

## Gap 4 — `select_filtered`: ≈ 13× behind MongoDB

**What this measures.** `SELECT ... WHERE` with a predicate against
an indexed column. Mongo's index path is direct; RedDB's secondary
indices currently snapshot at `CREATE INDEX` time and don't
auto-update on insert/update/delete, so the bench falls back to a
full-table scan that deserialises every row.

**Cited session.** `sess-20260416215030-1190998` (mongo lock window).
`BASELINE.md` aggregate: RedDB 113 ops/s vs Mongo 1,491 ops/s,
PG 683 ops/s.

**Closure issues.**

- [#156 — UnifiedRecord schema-shared layout](../../issues/156-unifiedrecord-schema-shared.md)
  (in flight). Cuts the per-row `HashMap` allocation that dominates
  scan CPU.
- [#160 — IncrementalIndexMaintainer](../../issues/160-incremental-index-maintainer.md)
  (in flight). Maintains secondary indices on every write so the
  query planner can use them, instead of re-scanning the segment.

**Reproducing.**

```bash
cd rdb-benchmark
make duel-official OFFICIAL_SCENARIOS=select_filtered OFFICIAL_DB=duel-core
```

## Picking an engine while these are open

| if your hot path is...                         | pick (today)         | revisit RedDB when |
|------------------------------------------------|----------------------|--------------------|
| highly concurrent OLTP                         | MongoDB / Postgres   | #157 re-benched    |
| large `UPDATE`s over warm leaves               | PostgreSQL           | #159 lands         |
| analytics / `GROUP BY` aggregates              | PostgreSQL           | #161 lands         |
| filtered `SELECT` with secondary indices       | MongoDB / Postgres   | #156 + #160 land   |
| typed bulk ingest                              | **RedDB**            | now — see [`wins.md`](wins.md) |
| compact on-disk write throughput               | **RedDB**            | now — see [`wins.md`](wins.md) |

## Updating this page

When a closure issue lands and the gap closes (or moves), update:

1. The `Cited session` line for the affected scenario.
2. The ops/sec values quoted from `BASELINE.md`.
3. The closure-issue link state (`in flight` → `shipped` → remove
   the row when the gap is gone).
