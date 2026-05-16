# ADR 0009 — Performance gate scope: universal 20% vs scenario-specific

**Status:** Accepted
**Date:** 2026-05-06
**Supersedes:** —
**Superseded by:** —
**Related issues:**
[#152](https://github.com/reddb-io/reddb/issues/152) (parent PRD: competitive perf push),
[#153](https://github.com/reddb-io/reddb/issues/153) (this ADR),
[#156](https://github.com/reddb-io/reddb/issues/156) (UnifiedRecord),
[#157](https://github.com/reddb-io/reddb/issues/157) (WAL lock-free),
[#158](https://github.com/reddb-io/reddb/issues/158) (Pager striped),
[#159](https://github.com/reddb-io/reddb/issues/159) (BTree batch),
[#160](https://github.com/reddb-io/reddb/issues/160) (IncrementalIndex),
[#161](https://github.com/reddb-io/reddb/issues/161) (AggregateQueryPlanner),
[#163](https://github.com/reddb-io/reddb/issues/163) (productize wins).

## Context

PRD #152 exists to make RedDB visibly competitive against PostgreSQL,
MongoDB, and Neo4j on a mapped set of benchmark scenarios. The PRD
needed a scope decision before it could be sized: what does
"competitive" actually mean across the full scenario matrix? Two
honest answers were on the table.

The reproducible numbers in `rdb-benchmark/BASELINE.md` make the
shape of the problem concrete. RedDB has real wins where the unified
engine is doing work other engines need a stack to do: `typed_insert`
runs ~16× faster than Postgres, `disk_usage` is ~1.5× tighter than
Mongo. Those wins are now productized in `docs/perf/wins.md`. RedDB
also has real gaps where storage-engine specialisation matters more
than unification: `concurrent` is ~49× behind Mongo, `bulk_update` is
~30× behind Postgres, `aggregate_group` is ~12× behind Postgres,
`select_filtered` is ~13× behind Mongo. Those gaps are catalogued in
`docs/perf/when-not-reddb.md`.

The shape of those gaps is not "tune the existing code". Closing
`concurrent` to within 20% of Mongo means a sharded log structure
beyond the WAL lock-free work in #157. Closing `aggregate_group`
means a columnar push-down execution path well beyond the
AggregateQueryPlanner in #161. Closing `update_random` means an MVCC
redesign beyond the pager striping in #158. Each of those is a
multi-quarter PRD on its own, with architectural commitments RedDB
has not yet made.

So PRD #152 had to settle a posture question before its slices made
sense: are we promising universal-20% as the public bar, or are we
promising something narrower and more honest? This ADR records that
posture decision.

## Decision

### Option B — Scenario-specific — chosen

RedDB commits to winning in the scenarios where the unified engine
is structurally doing more work than the comparator stack —
`typed_insert`, `disk_usage`, queries that cross models — and to
parity-or-close-gap in scenarios where storage-engine specialisation
dominates. The closure target on the gap scenarios is "within a
factor that does not look broken in a benchmark grid", not "≥20%
ahead".

This is the posture PRD #152 ships against. The roadmap items in the
PRD — #156 UnifiedRecord, #157 WAL lock-free, #158 Pager striped,
#159 BTree batch upsert, #160 IncrementalIndex, #161
AggregateQueryPlanner — are sized to defend the wins and narrow the
gaps without committing the org to the architectural rework that
universal-20% would require. The new docs slices (#163 productize
wins, this ADR) make the posture readable from outside.

### Option A — Universal 20% — rejected

The universal posture would have read better in marketing copy: a
single sentence that holds across the scenario grid. Rejected
because it would invalidate the bounded scope of PRD #152. To
defend it, RedDB would need a sharded log structure (not the WAL
lock-free improvement #157 ships), a columnar push-down planner (not
the AggregateQueryPlanner #161 ships), and an MVCC redesign (not the
pager striping #158 ships). Each is a multi-quarter PRD. None of
them are scheduled. Promising the bar before scheduling the work
would over-commit the engine on architectural posture it has not
chosen.

### Rationale

The unified-engine wins are real, reproducible, and explainable from
first principles: a single durable engine that already holds typed
records, indexes, and JSON values pays less per write than a
client-driver-server-storage stack assembling the same record.
That story holds in the benchmark and in the architecture. RedDB
should productize it.

The gaps are also real and explainable from first principles:
single-purpose storage engines beat unified ones on workloads they
were specialised for. Pretending otherwise — claiming universal-20%
without the architectural commitments to defend it — would over-
promise. Honest positioning costs less than missed claims.

## Consequences

**Benefits.**

- The scope of PRD #152 stays bounded. Each slice (#156–#161) is a
  weeks-of-work item, not a multi-quarter rewrite. The PRD is
  deliverable.
- Public positioning matches reproducible numbers. `docs/perf/wins.md`
  documents where RedDB beats the comparator stacks; `docs/perf/when-not-reddb.md`
  documents where it does not. Readers do not have to discover the
  gap scenarios on their own.
- Productized wins become the headline narrative. The unified-engine
  insert path and disk-footprint advantage are claims RedDB can make
  without footnotes.
- The decision is auditable. A future contributor proposing the
  sharded log / columnar push-down / MVCC redesign work knows the
  posture they are reopening, and the closure-trigger conditions
  recorded below are the place to start.

**Costs.**

- The "≥20% faster than PG/Mongo/Neo4j" claim is dropped from public
  marketing. RedDB does not advertise a scenario-by-scenario universal
  lead. The marketing copy that would have used that line has to be
  rewritten against the wins/gaps split instead.
- Two surfaces — `docs/perf/wins.md` and `docs/perf/when-not-reddb.md`
  — must be kept honest as the engine evolves. A scenario that closes
  has to migrate from `when-not` into `wins` (or at least out of
  `when-not`); a regression has to migrate the other way.
- Reviewers of future PRDs that touch performance scope have to
  re-read this ADR before advertising "RedDB beats X" claims that
  cross the scenario boundary.

**Open questions.**

- When does the org reconsider Option A? The closure-trigger
  conditions are: a gap scenario comes within ~2× of the comparator
  on a reproducible run, and the next architectural step to close
  the remaining factor fits inside a single PRD. When both hold for
  a scenario, that scenario is a candidate to migrate from
  `when-not-reddb.md` into a proper "we now win here" narrative —
  and if enough scenarios cross that line, the universal posture is
  worth re-opening. Not before.
- Whether the scenario-specific posture should be advertised as a
  capability matrix on the public site, or kept in the docs tree
  where engineers find it. Productization of the wins (#163) is the
  first move; site-level positioning is downstream of that.
- Whether parity-or-close-gap on the gap scenarios needs an explicit
  numeric target per scenario (e.g. "within 3× of Mongo on
  `concurrent`"), or whether the qualitative bar is enough until a
  customer asks. Deferred — the slices in PRD #152 do not depend on
  the answer.

## Cross-links

- Parent PRD: [#152](https://github.com/reddb-io/reddb/issues/152)
- Wins narrative: `docs/perf/wins.md`
- Gaps narrative: `docs/perf/when-not-reddb.md`
- Benchmark numbers: `rdb-benchmark/BASELINE.md`
- Roadmap: `docs/perf/roadmap.md`
- In-flight closure slices: [#156](https://github.com/reddb-io/reddb/issues/156),
  [#157](https://github.com/reddb-io/reddb/issues/157),
  [#158](https://github.com/reddb-io/reddb/issues/158),
  [#159](https://github.com/reddb-io/reddb/issues/159),
  [#160](https://github.com/reddb-io/reddb/issues/160),
  [#161](https://github.com/reddb-io/reddb/issues/161).
