# Multimodel building-block implementation ledger

RedDB supplies database primitives that applications compose through public APIs.
The target is the expressive power of SurrealDB with stronger measured performance
and equally explicit correctness guarantees. No application-specific rules belong
in the engine. This ledger records the approved program; it is not a completion claim.

Reference source: local SurrealDB main at
`18971ffba781a796c0ad3897bb4b82432c241e4c`. This is an architectural reference,
not the stable-release baseline for competitive measurements. RedDB work starts
at `ecc774a0e76378c9f444da02c04f10d195a9580c`, preserving the vector publication,
rollback/reopen and scoring-lock corrections in draft PR #2295.

## Implementation sequence and acceptance

| Capability | Current delivery | Remaining acceptance |
|---|---|---|
| Collection final-record expressions | First slice: TABLE stored generated columns and column CHECK using the existing scalar evaluator and persisted declared contract | Virtual fields; schema contracts for all applicable models; transactional ALTER/backfill; expression planning cost measurements |
| Types, defaults and constraints | Reuse existing normalization; expression results pass through type/NOT NULL enforcement before uniqueness and index maintenance | Referential constraints across models, stable logical references, RESTRICT default and explicit CASCADE/SET NULL; transaction write-set validation |
| Persisted RQL functions | Initial sequential implementation under validation: CREATE/ALTER/DROP, CALL, typed arguments/results, SHOW/export, invoker permissions, pure/read/write admission, and cooperative CALL budgets for relational scans/joins, exact/TurboQuant search, graph materialization/pattern expansion and hybrid fusion | Recursive composition; interruptible blocking/atomic operations, parallel budget propagation and peak-memory accounting; transactional catalog DDL; transport parity and catalog compaction. See [stored functions](../query/stored-functions.md) for supported operations and limits |
| Collection rules | Pending | Named conditions/actions; synchronous source and derived writes in one transaction; asynchronous work enqueued durably in the same transaction; rollback/savepoint coverage |
| Durable streams and change subscriptions | Existing in-process streams are insufficient | Committed event identity, disk log and offsets, public append/read/cursor/retention; explicit expired-cursor error; consistent bootstrap, filter/projection/cancel on one collection |
| Declarative HTTP endpoints and SDK parity | Pending | Method/path/parameters bind to named function; GET read-only; common auth/transaction path; schemas/docs derived from definitions; CALL parity and definition export/import |
| Transaction/WAL convergence | Preserve and extend the live optimistic engine; restore context and secondary indexes on rollback/savepoints and use logical identity for repeated upserts | Common validate→WAL→durability→publication→ack path; synchronous durable defaults and snapshot isolation; indeterminate commit result; group commit; idempotent replay, checksums, consistent checkpoints and full-page-image tests |
| Retention and corruption handling | Pending systematic audit | Incomplete WAL suffix versus internal corruption; readers/backups/PITR/replicas included in retention floor; optional long-term history separate from active MVCC |
| Query performance | Prior vector improvements retained; [runtime graph kind filtering](../perf/graph-materialization-filter.md) avoids unrelated payload copies and CALL candidate IDs, including in mixed collections; [mixed bulk kind indexing](../perf/mixed-bulk-kind-index.md) corrects type lookup and removes its ID-set copy; [graph segment pruning](../perf/graph-segment-pruning.md) uses conservative presence across mutation; [logical graph identity](graph-logical-identity.md) preserves runtime paths through retained MVCC versions; [native graph reads](native-graph-identity.md) share snapshot/RLS/identity and preserve projections; [search graph expansion](search-graph-expansion.md) shares snapshot, RLS, logical identity and collection scope with maintained per-segment adjacency/version probes and lazy node hydration; [context vector expansion](search-vector-expansion.md) reuses selected authorized payloads without global rehydration; [exact vector cursors](vector-scan-cursor.md) replace per-query candidate ID lists with captured segment positions and bounded batches; [segment lifetime accounting](retained-segment-memory.md) includes retired sources and in-flight merge work in admission; [concurrent reservations](concurrent-memory-reservations.md) prevent existing runtime admissions from sharing the same headroom; [row index admission](row-index-memory-admission.md) reserves secondary/implicit index growth before publication and prevents composite insert fan-out | Shared peak-memory admission and bounded high-degree expansion for graph search; extend bounded cursors to remaining scans; complete growth estimates, remaining write-path admission and maintenance headroom; projection/filter pushdown; common multimodel snapshot/auth and bounded memory |
| Three complete public examples | Pending; initial schema examples accompany expression slice | Agent knowledge, fraud/events, commerce; only public APIs; graph/vector/document composition and failure paths |
| Competitive acceptance | Previous diagnostic measurements preserved | Pinned stable competitors, equivalent durability/results, workload gates below; no superiority claim before these pass |
| Migration | Explicit destination required when incompatible changes are needed | Preserve source; validate data/schema/policies/references/history/queue state; do not downgrade expression-bearing databases into older binaries that ignore unknown schema metadata |

Collection, record and field policies must apply equally to queries, functions,
rules and endpoints. Queue jobs, durable streams, transient notifications and
query-result streaming remain distinct APIs. First change subscriptions do not
promise arbitrary incremental joins.

## Executable matrix

`tests/grouped/schema_query_core/e2e_collection_expressions.rs` exercises the
first capability through SQL and the public native application API:

| Model | Operation | Surface | Correctness/recovery assertion |
|---|---|---|---|
| TABLE | INSERT, UPDATE | RQL | Defaults, forward generated dependencies, CHECK on final record; index old/new keys; clean reopen and subsequent writes |
| TABLE | INSERT, PATCH, bulk | Native API | Same derived values and validation; invalid batch leaves no prefix |
| TABLE | Bulk INSERT/UPDATE | RQL | Invalid input leaves no partially applied statement |
| TABLE | Upsert | RQL | Conflict update recomputes derived fields and checks the final row |
| TABLE | Explicit transaction/savepoint | RQL | Read-own-write and reverse-order rollback of repeated updates; restore equality/index candidates; generated UNIQUE conflict |
| TABLE | CREATE/ALTER | RQL | Invalid dependencies/types/effects leave no collection; unsupported backfill fails explicitly |
| TABLE | SHOW CREATE/import | RQL | Exported definitions retain generated expressions and CHECK; imported schema enforces the same rules |
| TABLE | Invalid persisted schema | Single-file and operational profiles | Refuse open on semantic contract errors; do not silently fall back to an older definition |
| Other models | All | All | Pending, not inferred from TABLE coverage |
| TABLE | All | HTTP/gRPC/SDK | Pending transport-specific execution, not inferred from native coverage |
| All models | Process/power failure | Persistent | Pending literal child-process kill and fault-VFS validation; clean reopen is not a crash test |

Run the matrix with:

```sh
CARGO_TARGET_DIR=/tmp/reddb-competitive-audit/cargo-target CARGO_BUILD_JOBS=2 \
  cargo test --locked -p reddb-io --test grouped_sql_core e2e_collection_expressions -- --test-threads=1
```

The parser and file codec have focused tests for expression boundaries, source
escaping, bounded depth, physical roundtrip and absent legacy metadata fields.
Future matrix rows need executable tests before changing their status.

## Expression slice contract and cost

Expressions are parsed once at DDL/load and shared using Arc. The initial effect
boundary admits local fields, literals, arithmetic/comparison/logical operators,
CAST, CASE, IN, BETWEEN and null predicates. Functions, external lookups,
parameters and subqueries are rejected. Input is limited to 64 KiB/1024 tokens,
AST depth to 64, with the existing parser nesting limit also applied.

Base values/defaults normalize first, generated fields evaluate in dependency
order, result types/NOT NULL normalize next, then CHECK validates the final
record. CHECK rejects FALSE; TRUE and NULL pass, per SQL semantics. Generated
values supplied by a caller or carried from an older version are replaced by the
expression result. All generated fields are currently recomputed on row mutation.
Rollback/savepoint undo processes version chains newest first and restores context
and secondary index candidates. Storage/index undo errors propagate after pending
effect cleanup; this is not a claim of recovery from arbitrary I/O failure.
Upsert translates the conflicting physical version into its
logical record identity before executing the update.

Cost sketch: no network round trips or additional fsyncs per expression; stored
results add their encoded width to ordinary row/WAL writes; CPU is O(E) expression
nodes plus O(G² + G×D) dependency scheduling and existing normalization per row,
with O(C + E) temporary/schema memory (C columns, G generated fields, D dependency
edges). Scheduling is currently per normalization; precompiled schema plans and
selective invalidation remain performance work, not measured achievements.

ALTER that changes an expression-bearing column schema, or adds a new expression,
is rejected until transactional backfill exists. Existing metadata without
expressions retains its previous meaning. Older binaries are not safe writers of
expression-bearing data, because they do not enforce the new contract.

## Performance and resilience gates

- SurrealDB: at least 20% greater throughput in each of the three complete
  workflows, with no worse p95/p99, identical results and equivalent durability.
- SQLite/PostgreSQL equivalent workloads: latency ≤1.10× comparator and throughput
  ≥comparator/1.10. Cassandra comparison belongs to the later distributed phase.
- Fixed shared embeddings; no paid AI/provider latency. Exact versus exact; ANN
  measured separately with recall ≥0.95.
- Seven paired repetitions, alternating order, separate warmup, at least 10,000
  operations per repetition, 95% confidence intervals; preserve failures/outliers.
- Current machine only; controlled load windows. Shared-load runs are diagnostic.
  Budget 4 GiB per engine, approximately 1 GiB resident/6 GiB logical datasets,
  concurrency 1/8/32, subject to disk-space checks before each campaign.
- Deterministic histories and scheduled interleavings, literal child-process kills,
  and existing VFS extended to model unsynced writes, partial writes, ENOSPC/EIO.

Ship independently reviewable capability PRs, with their evidence. Draft PRs and
pushes are authorized; main merge and release remain outside this implementation.
