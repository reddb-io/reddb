# ADR 0076 — Competitive leadership across the comparable matrix

**Status:** Accepted
**Date:** 2026-09-07
**Supersedes:** [ADR 0009](0009-performance-gate-scope.md)
**Related:** [Issue #2270](https://github.com/reddb-io/reddb/issues/2270)

## Context

The maintainer chose all three comparison fronts: foundations and maturity,
developer/product experience, and multimodel capabilities. The objective is
leadership throughout the comparable SurrealDB matrix, with SQLite included
for equivalent embedded workloads. Closing selected gaps without overtaking
the comparator no longer satisfies the objective. Structural changes and ADR
revisions are permitted when experiments establish the benefit and migration
cost. The selected performance margin is 20% per scenario.

This is a target, not a claim that the current release meets it. Issue #2270
requires measuring the actual application SDK, including its subprocess, and
checking successful writes through readback and recovery. Existing benchmark
labels or internal microbenchmarks do not prove that end-to-end contract.

## Decision

1. **Use a fixed, versioned comparison matrix.** Cover foundations, product
   experience, and multimodel composition. Record every applicable cell as
   unmeasured, invalid, behind, inconclusive, parity, or demonstrated lead.
   Keep unfavorable cells visible. An unsupported comparator capability is
   not a performance victory. Edition, backend and platform limitations are
   explicit dimensions, not reasons to silently remove a cell.
2. **Fix the primary metric before measuring.** Latency-led cells require
   RedDB/comparator <= 0.80; throughput-led cells require >= 1.20. Require a
   95% confidence interval entirely on the winning side of that threshold,
   using independent run-level samples rather than pretending correlated
   operations within a run are independent experiments. Report p50/p95/p99,
   throughput, errors, CPU, memory and disk alongside the primary metric.
   A throughput increase of 20% and a latency reduction of 20% are different
   thresholds; do not interchange them.
3. **Correctness and equivalent guarantees precede speed.** Match query
   results, transaction boundaries, effective isolation and acknowledged
   durability. Match retrieval quality when measuring approximate search.
   Verify all expected keys and values, reopen persistent stores, and test
   failure recovery separately. Invalid runs cannot count as fast runs.
   SQLite WAL/NORMAL and an fsync-per-commit engine are separate durability
   classes until explicitly normalized; tmpfs is a diagnostic filesystem,
   not proof of durable performance.
4. **Measure real product paths.** Separate Rust in-process, JavaScript
   in-process, JavaScript subprocess/IPC, and remote transports. Publish the
   SDK and the engine actually embedded in it, not merely the server's
   release number. Record binaries/checksums, revisions, indexes, dataset,
   hardware, concurrency, warmups, repetitions and maintenance activity.
   Use exclusive comparable hosts for official claims. Shared-host results
   remain diagnostic evidence even if their confidence intervals look good.
5. **Evaluate product completion and composition explicitly.** Run the same
   documented user tasks; record success, elapsed time, steps, additional
   application code, error clarity, and transport/platform coverage.
   Feature counts and a global average do not conceal failed journeys.
6. **Reopen architecture with evidence.** Every proposed change identifies
   the measured bottleneck, the alternative, expected benefit, compatibility
   and migration effects, and the proving experiment. Nothing here selects
   N-API, a new storage engine, a protocol rewrite or weaker durability.
   Data-plane proposals retain STYLE.md's resource-cost sketch requirement.

## Consequences

The old scenario-specific posture is historical. Its benchmark results must
retain their dates and execution semantics until revalidated. Public claims
must link to qualifying evidence; roadmap ambition cannot be presented as
shipped performance. The initial audit and the benchmark harness establish
the baseline and an implementation backlog; passing the whole matrix is a
subsequent engineering program, with every remaining gap explicitly tracked.

Performance regressions must be evaluated against the same fixed workload
and guarantees. A failing cell remains a release/roadmap concern; it is never
made green by relabeling the workload or averaging it with unrelated wins.

## Alternatives

- Preserve selective wins and parity-or-close-gap: rejected by the maintainer.
- Count any statistically significant advantage: rejected in favor of a
  material 20% margin.
- Rewrite before profiling: rejected; correctness and migration costs need
  concrete evidence, particularly in WAL, MVCC, tenancy and wire contracts.
