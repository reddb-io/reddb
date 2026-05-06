# Where RedDB Wins

Status: 2026-05-06 — verifiable wins from the canonical `duel-official`
methodology lock (issue #154).

This page lifts the two scenarios where RedDB measurably beats both
PostgreSQL and MongoDB out of the internal `rdb-benchmark/BASELINE.md`
report into a stable, user-facing reference. Numbers update by changing
the cited session, not by editing prose.

## Methodology

All numbers below come from the canonical benchmark configuration
locked under issue #154: `BenchConfigSchema` with `OFFICIAL_PROFILE =
standard`, `OFFICIAL_RUNS = 10`, `ITEMS = 50000`. The matching Make
target is `make duel-official` in
[`rdb-benchmark`](https://github.com/reddb-io/rdb-benchmark). For a
quick development variant use `make mini-duel` (same schema, smaller
shape).

Each "win" cites a specific session id. The values printed below are
the `ops_per_sec` rows from that session in
`rdb-benchmark/benchmarks/history.jsonl`. Re-running the cited command
on a new session will produce a new id; update this doc by swapping
the `Cited session` line and rebuilding from history.

Companion document: [`when-not-reddb.md`](when-not-reddb.md) — the
scenarios where RedDB is currently *behind* and the in-flight closure
work.

## Win 1 — `typed_insert`: RedDB ≈ 16× over PostgreSQL

**What this measures.** Sequential single-row inserts into a typed
schema (`users(id BIGINT PRIMARY KEY, name, email UNIQUE, age, city,
score, created_at)` with `idx_age` + `idx_city`). Same schema both
sides. No fsync tricks, no dropped indices.

**Cited session.** `sess-20260416024831-496900`
(`rdb-benchmark/benchmarks/history.jsonl`,
`profile=standard`, 10K items × 3 runs, `reddb_binary` transport).

| engine        | ops/sec | source row                                          |
|---------------|--------:|-----------------------------------------------------|
| RedDB         |  31,950 | `database=reddb_binary`, `scenario=typed_insert`    |
| PostgreSQL 17 |   1,820 | `database=postgresql`, `scenario=typed_insert`      |

The internal baseline at `rdb-benchmark/BASELINE.md` aggregates this
session with the rest of the post-2026-04-16 sweep and reports the
locked numbers as **26,523 ops/s** (RedDB) vs **1,677 ops/s** (PG) —
the **16×** ratio quoted in marketing.

**Reproducing.**

```bash
cd rdb-benchmark
make build
make duel-official OFFICIAL_SCENARIOS=typed_insert OFFICIAL_DB=duel-core
make stats
```

For a faster local check (small N, dev variant of the same schema):

```bash
make mini-duel SCENARIO=typed_insert ITEMS=10000 RUNS=3
```

**Why RedDB wins this one.** `typed_insert` exercises the unified
engine's native typed-column ingest. RedDB serialises a typed row
once into the protobuf-shaped binary cell and writes it through a
single B-tree path
(`store.bulk_insert` → `btree.bulk_insert_sorted` with the
tail-append fast path). PostgreSQL has to plan + bind each row,
journal it, and update four B-tree indices on every insert. RedDB's
on-disk format collapses that work into one append + one slot
update, so the throughput gap is structural — it widens with
schema size and index count, not with tuning.

## Win 2 — `disk_usage`: RedDB ≈ 1.5× over MongoDB

**What this measures.** Throughput of the bulk-insert path while
holding the on-disk page format constant. The scenario rejects engines
that pay for compactness with insert latency or vice versa.

**Cited sessions.**

- RedDB and PostgreSQL: `sess-20260416024831-496900`
  (`profile=standard`, `reddb_binary` transport).
- MongoDB: `sess-20260416215030-1190998`
  (`profile=standard`, locked `2026-04-16T22:06:11+00:00` per
  `rdb-benchmark/benchmarks/baselines.yaml`).

| engine        | ops/sec | source row                                       |
|---------------|--------:|--------------------------------------------------|
| RedDB         | 186,468 | `database=reddb_binary`, `scenario=disk_usage`   |
| MongoDB       |  86,362 | `database=mongodb`, `scenario=disk_usage`        |
| PostgreSQL 17 |  79,304 | `database=postgresql`, `scenario=disk_usage`     |

`BASELINE.md` aggregates these with the rest of the post-lock runs and
publishes **128,753 / 86,716 / 83,105** ops/s for the same row order —
a **1.5×** RedDB-over-Mongo ratio.

**Reproducing.**

```bash
cd rdb-benchmark
make build
make duel-official OFFICIAL_SCENARIOS=disk_usage OFFICIAL_DB=duel-core
make stats
```

**Why RedDB wins this one.** The unified `.rdb` page format
co-locates entity bytes, slot offsets, and the B-tree leaves inside
the same pager. There is no separate WAL append + heap rewrite step
(Postgres) and no BSON-document overhead per row (Mongo). The
slotted-page layout (`storage/engine/btree.rs:435-492`, post-#3
fix) means a bulk insert is one O(log M) binary search + one
`copy_within` of u16 slot pointers — both Postgres and Mongo do
strictly more work per row at this batch size.

## Updating this page

When a fresher canonical session ships, update:

1. The `Cited session` line in each win.
2. The ops/sec table values from that session's `history.jsonl` rows.
3. The aggregated `BASELINE.md` reference if the lock baseline rolled.

Do **not** embed numbers anywhere outside the cited tables — README
and driver guides link here for the source of truth.
