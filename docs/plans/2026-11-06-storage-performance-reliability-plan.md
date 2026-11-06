# Storage performance and reliability plan

This program applies lessons from PostgreSQL, Redis and Cassandra to RedDB's
existing collection models. A mechanism is not considered complete merely
because a primitive exists: its runtime path, recovery behavior, resource use
and operator visibility must be demonstrated. Do not introduce a new logical
collection model solely to implement these optimizations.

## Common gates

- Record a baseline on the same machine, dataset, binary, durability mode and
  deployment profile; retain unfavorable results and exact commands. Report
  p50/p95/p99, throughput, I/O, memory, disk and WAL bytes where relevant.
- Preserve MVCC visibility, transaction and WAL ordering, tenancy, RLS,
  replication authority, queue delivery/ACK/retry and wire compatibility.
- Test checkpoint and interrupted recovery, an additional cold boot, and stable
  file size under repeated restarts. Make failures observable rather than
  returning incomplete success.
- Maintenance must be paced and bounded per ADR 0038; file contracts belong
  to `reddb-file` per ADR 0046. Compare performance without relaxing durability
  or correctness, and use ADR 0076 for competitive claims.

## 1. Append-only retention and cold boot

Existing: append-only segments have retention bounds, manifest retirement and
recovery tests; the runtime sweeper already retires expired segments.

- Audit the source of truth for each deployment profile and whether pinned
  readers, snapshots, backups, forks or replicas can still reference a segment.
  Establish the precise safe retirement and reclamation fences before changing
  deletion behavior. Cover late arrivals, mixed/unknown timestamps, cutoff
  boundaries and clock changes.
- Bound discovery and retirement work per tick, including manifest reads and
  publications; make backlog, failure and blocking reasons observable.
- Verify crash points before/after each publication, repeated recovery and
  cold boot, exact retained data and file sizes. Compare many small segments
  with one large segment and sustained ingestion during retirement.

## 2. Index-only queries and planner

Existing: visibility maps, zone maps, Bloom filters and index-only planning
helpers exist. Inventory which runtime and cold disk-resident paths use them.

- Test covering and non-covering projections on tables and documents, including
  old snapshots, concurrent writes, deletes, rollback, RLS and reopen. Only
  skip data reads when the index and visibility proof support the full query.
- Measure actual page reads and output equivalence versus the ordinary path;
  check invalidation and index lifecycle after restart.
- Evaluate correlated-column statistics on multi-tenant skew with measured
  cardinality estimates and plan choices. Add statistics only where the
  evidence shows a gap; account for update and memory costs.

## 3. KV/cache expiration and WAL

- Exercise staggered and simultaneous expirations under reads and writes,
  memory pressure and restart. Verify exact deadline behavior, TTL changes,
  replication/promotion and isolation from Config and Vault history.
- Measure p99, backlog, work per tick, WAL and file growth. Bound cleanup and
  retain authoritative expiry on reads even when background work is delayed.
- Measure checkpoint plus WAL replay time and space under churn. Evaluate
  incremental checkpoints or segmented replay only when they improve the
  measured bottleneck; prove publication and crash ordering for every mode.

## 4. Queue delivery and operations

- Verify persisted indexes and bounded candidate reads under enqueue, delayed
  delivery, consumer groups, ACK, NACK, lease expiry, retry and DLQ, including
  concurrent consumers, aborts, crashes and cold reopen.
- Expose useful pending counts, oldest age, retry and lag without full payload
  scans. Check tenant/RLS isolation and compare index results with an oracle.
- Benchmark p99 and I/O as queue history grows while the live working set stays
  fixed; verify index and file growth plateau after cleanup.

## 5. Segment pruning and Bloom filters

- Trace persisted min/max and Bloom metadata from publication to actual cold
  reads for logs, time series and append-only tables. Compare every optimized
  result with a full-scan oracle, including false positives, corrupt or missing
  metadata, empty segments and values outside summaries.
- Report files/pages skipped, false-positive rate, memory footprint and cold
  boot work; never use a probabilistic positive as proof of presence.

## 6. Replication, tombstones and repair

- Document deletion and reclamation horizons for each supported replication
  profile, including retained snapshots, stale replicas, rejoin and promotion.
  Detect when a replica cannot safely catch up and require the existing safe
  rebootstrap path; do not infer safety from elapsed time alone.
- Test offline replicas returning before and after reclamation, interrupted
  recovery, network partitions and acknowledged writes. No deleted value may
  reappear and no valid snapshot may lose its visible version.

## 7. Maintenance observability and end-to-end qualification

- Surface backlog, progress, last error, retained bytes and blocking horizon
  for maintenance and recovery without expensive full scans.
- Run the relevant storage fault and growth lanes, persistent multimodel tests
  and reproducible workload matrix across embedded and disk-resident profiles.
  Publish commands, environment and results with each independently reviewable
  change. Mark each item done only after its correctness and measurement gates
  pass; implementation order may change when evidence identifies a blocker.
