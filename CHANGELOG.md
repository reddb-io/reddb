# Changelog

All notable changes to RedDB are documented here. Dates are ISO-8601 (UTC-3).

## [Unreleased]

## [0.2.4] - 2026-04-26

### Added

- **Architectural deepening (6 clusters):**
  - `service_router::ProtocolDetector` trait + composable `Router` — replaces the centralised probe match; adding a new protocol becomes a new struct, not edits to a switch.
  - `runtime::lease_lifecycle::LeaseLifecycle` — single owner of the writer-lease state machine. Eliminates drift between `WriteGate.set_lease_state` and `AuditLogger.record`. `WriteGate::set_lease_state` is now `pub(crate)`.
  - `storage::backend::AtomicRemoteBackend` trait — splits CAS contract off `RemoteBackend`. `LeaseStore::new` requires `Arc<dyn AtomicRemoteBackend>`. Turso/D1 cannot be wired into a writer lease; the type system rejects them. `AtomicHttpBackend` wraps `HttpBackend` and refuses construction unless `RED_HTTP_CONDITIONAL_WRITES=true`.
  - `Pager` owns page-level encryption directly: `PagerConfig::encryption: Option<SecureKey>`, transparent `read_page_decrypted` / `write_page_encrypted`, fail-closed marker matrix (encrypted-vs-plain × key-supplied-vs-absent). `EncryptedPager` collapses to a `#[deprecated]` thin wrapper.
  - `server::transport::run_use_case` + `map_runtime_error` — single source of truth for `RedDBError → HTTP status`. Migrated 8 endpoints; rest stay on the manual pattern by design (audit lifecycle, multi-step orchestration).
  - `application::OperationContext` + sealed `WriteConsent` token. `WriteGate::check_consent` is the only mint site outside the gate module. Six `Runtime*PortCtx` extension traits ship default-forward methods; pilot threads context through `handlers_entity::handle_scan/create_row/create_node`.
- Backend conditional-write contract for writer leases: `RemoteBackend` exposes object version tokens plus conditional upload/delete. Local FS uses content-hash tokens; S3-compatible stores use ETag + `If-Match`; HTTP opts in with `RED_HTTP_CONDITIONAL_WRITES=true`.
- Release gates for cold-start baselines, artifact size measurement, feature-matrix compilation, and nightly backup/restore drills.
- `reddb_slo_lag_budget_remaining_seconds{replica_id}` metric, derived from `RED_SLO_REPLICA_LAG_BUDGET_SECONDS` and replica lag.
- CI publish dry-run job: `cargo package` (engine) + `cargo package --no-verify` (rust client) on every PR. Catches packaging issues before release time.

### Changed

- Serverless writer fencing fails closed when the configured backend cannot enforce CAS — no more last-writer-wins on writer leases.
- Production release binaries use `panic = "abort"`; WAL/recovery is the consistency boundary after process death.
- `Cargo.toml` `include` patterns anchored with leading `/` so `README.md` / `LICENSE` no longer sweep up vendored `node_modules` / `dolt` trees. Package shrinks from 1026 → 638 files.
- Engine version bumped 0.1.5 → 0.2.4 to align with crates.io's 0.2.x line. `drivers/rust` engine dep follows.

### Fixed

- `drivers/rust/src/embedded.rs` adapted to the engine's `Arc<str>` schema-key types — embedded mode compiles again.

### Documentation

- Added `docs/release/v1.0-migration.md`, `docs/reference/features.md`, `bench/artifact-sizes.md`, and `docs/release/drill-history.md`.
- Updated the operator runbook with the lease backend/runtime matrix, panic policy, and SLO lag budget alert.

### Changed

- Serverless writer fencing now fails closed when the configured backend cannot enforce compare-and-swap. A failed lease acquire is preferred over split-brain.
- Production release binaries use `panic = "abort"`; WAL/recovery is the consistency boundary after process death.

### Documentation

- Added `docs/release/v1.0-migration.md`, `docs/reference/features.md`, `bench/artifact-sizes.md`, and `docs/release/drill-history.md`.
- Updated the operator runbook with the lease backend/runtime matrix, panic policy, and SLO lag budget alert.

## [v1.0-rc1] - planned

### Breaking Changes

- Public cloud/runtime configuration uses the `RED_*` namespace. Legacy `REDDB_*` aliases remain accepted where already shipped, but new deployment manifests should use `RED_*`.
- Writer lease deployments must use a CAS-capable backend. Filesystem leases are not supported on ephemeral runtimes or NFS-style shared filesystems for v1 production fencing.
- Encryption-at-rest is **foundation-only** in v1.0 unless a later release note explicitly says the pager format was bumped and wired. Do not market v1.0 as encrypted-at-rest by default.

### Migration Guide

- See `docs/release/v1.0-migration.md`.

## 2026-04-23 — REST surface rewrite: RESTful + collection-centric

Dropped the `/vcs/*` RPC-shaped endpoints and replaced them with a
properly RESTful, collection-centric layout. **Breaking change for
HTTP consumers** — there is no compatibility shim.

### Design

- **Nouns, not verbs**: `/repo/commits`, `/repo/refs/heads/{name}`,
  no `/vcs/checkout` or `/vcs/merge` as top-level paths.
- **HTTP semantics respected**: GET for reads, POST creates,
  PUT moves refs, DELETE deletes. `201 Created`, `204 No Content`,
  `404 Not Found`, `409 Conflict` mapped from RedDBError.
- **Session-scoped state transitions**: checkout/merge/reset/
  cherry-pick/revert live under `/repo/sessions/{conn}/*` instead
  of taking `connection_id` as a body field on a global endpoint.
- **Collection-centric opt-in**: `/collections/{name}/vcs` owns
  the versioned toggle — that's how a dev thinks about it, the
  collection is the resource and VCS is an aspect.
- **Nested conflicts**: `/repo/merges/{msid}/conflicts/{cid}/resolve`.
- **Consistent JSON envelope**: `{ ok, result }` / `{ ok, error }`
  unchanged.

### New surface (20 endpoints)

```
GET    /repo                                     repo summary
GET    /repo/refs[?prefix=…]                     unified ref listing
GET    /repo/refs/heads                          branch list
POST   /repo/refs/heads                          create
GET    /repo/refs/heads/{name}                   show
PUT    /repo/refs/heads/{name}                   move ref
DELETE /repo/refs/heads/{name}                   delete
GET    /repo/refs/tags
POST   /repo/refs/tags
GET    /repo/refs/tags/{name}
DELETE /repo/refs/tags/{name}
GET    /repo/commits?branch=&limit=&…            log
POST   /repo/commits                             create (session workset)
GET    /repo/commits/{hash}                      show
GET    /repo/commits/{a}/diff/{b}                diff
GET    /repo/commits/{a}/lca/{b}                 LCA
GET    /repo/sessions/{conn}                     status
POST   /repo/sessions/{conn}/checkout
POST   /repo/sessions/{conn}/merge
POST   /repo/sessions/{conn}/reset
POST   /repo/sessions/{conn}/cherry-pick
POST   /repo/sessions/{conn}/revert
GET    /repo/merges/{msid}                       merge-state summary
GET    /repo/merges/{msid}/conflicts             list
POST   /repo/merges/{msid}/conflicts/{cid}/resolve
GET    /collections/{name}/vcs                   opt-in state
PUT    /collections/{name}/vcs                   toggle { versioned }
```

### Removed

Every `/vcs/*` endpoint from the previous layout. No migration
shim — callers update to the new paths.

### Bonus: cherry-pick + revert now have HTTP handlers

Previously runtime-only. Exposed via
`POST /repo/sessions/{conn}/{cherry-pick|revert}`.

### Docs

- `docs/vcs/overview.md` — operation matrix rewritten with new
  paths
- `docs/vcs/commands.md` — new REST section with cheat sheet,
  end-to-end example, and status code mapping
- `docs/vcs/walkthrough.md`, `docs/guides/git-for-data.md` — all
  curl examples updated

## 2026-04-23 — Git for Data: ALTER TABLE SET VERSIONED (Phase 7.1)

`ALTER TABLE <name> SET VERSIONED = true|false` now wires the VCS
opt-in flag through the standard DDL parser and executor, mirroring
`SET APPEND_ONLY = ...`. Parses identically, dispatches to
`vcs_set_versioned` at execute time, returns a human-readable
`versioned enabled on '<name>'` message.

Works retroactively: a table that already has rows and existing
commits in its history can be flipped in-place and the earlier
commits become queryable via `AS OF COMMIT '<hash>'` immediately
(as long as those commits' xids are still pinned, which is the
default — VACUUM doesn't reclaim pinned versions).

Tests (tests/e2e_vcs_opt_in.rs, 3 new):
  - alter_table_set_versioned_opts_in
  - alter_table_set_versioned_false_opts_out
  - as_of_works_after_opt_in_retroactively  (demonstrates the
    retroactive flow end-to-end)

Total e2e_vcs_opt_in coverage: 9 cases.

## 2026-04-23 — Git for Data: opt-in per collection (Phase 7)

Follow-up to the VCS ship. User collections now stay outside VCS
by default; each one explicitly opts in via
`vcs.set_versioned(name, true)` (library), `POST /vcs/versioned`
(REST), or `red vcs versioned on <name>` (CLI). This keeps
transactional churn (sessions, caches, queues) out of the commit
graph so VACUUM stays free to prune aggressively.

### Behaviour changes

- **Default = non-versioned**: a freshly-created collection does
  not participate in merge / diff / AS OF. No extra row versions
  pinned by commits referencing it.
- **`vcs_diff` / `vcs_merge` / cherry-pick / revert**: only scan
  opted-in collections when computing deltas and conflicts.
- **`AS OF` on unversioned table**: raises
  `AS OF requires a versioned collection — \`X\` has not opted in.`
  Internal `red_*` collections are an exception — they're
  append-only and always accept `AS OF`.
- **Idempotent + reversible**: `set_versioned(name, true)`
  twice is a no-op; `false` removes the flag.
- **`red_*` collections cannot be opted in**: the `red_vcs_settings`
  writer refuses them explicitly.

### New surface

- `VcsUseCases::set_versioned / list_versioned / is_versioned`
- `GET /vcs/versioned` → list; `POST /vcs/versioned` → set
- `red vcs versioned [list|on|off|check]`

### New collection

- `red_vcs_settings` — one row per opted-in user collection
  (`_id = name`, `versioned = true`, `ts_ms`). Added to
  `vcs_collections::ALL` so bootstrap creates it.

### Tests

- `tests/e2e_vcs_opt_in.rs` — 6 cases covering default off, opt-in,
  opt-out, idempotency, internal collection rejection, AS OF
  enforcement, post-opt-in AS OF success.
- All 34 existing e2e_vcs_* tests still green.

## 2026-04-23 — Git for Data (VCS layer over MVCC)

First-class version control on top of the MVCC engine. Every
mutation already runs under MVCC snapshots; the VCS layer pins
those snapshots, hashes them, and exposes git-style semantics
(commit / branch / tag / checkout / merge / cherry-pick / revert
/ reset / log / status / diff / LCA / AS OF time-travel) across
the CLI, REST, and SQL surfaces.

### New: seven internal `red_*` collections

Created on first boot, seeded with defaults under `red.vcs.*`.

- `red_commits` — commit entities (hash, root_xid, parents,
  height, author, committer, message, timestamp_ms, signature?)
- `red_refs` — branches (`refs/heads/*`), tags (`refs/tags/*`),
  per-connection `HEAD:<conn>` pointers
- `red_worksets` — per-connection working / staged state
- `red_closure` — commit ancestry index for fast LCA
- `red_conflicts` — shadow docs for unresolved merge conflicts
  (base/ours/theirs JSON + conflicting paths)
- `red_merge_state` — in-progress merge / cherry-pick / revert
  metadata
- `red_remotes` — remote repository configuration (Phase 7
  placeholder)

### MVCC extension: snapshot pin / unpin

- `SnapshotManager::pin(xid)` / `unpin(xid)` / `is_pinned(xid)`
  / `pin_count(xid)` — reference-counted so higher-level objects
  (commits, replica snapshots) coexist with each other.
- `prune_aborted` now skips pinned xids so historical row
  versions survive VACUUM as long as a commit references them.

### Runtime: full VCS surface

`impl RuntimeVcsPort for RedDBRuntime` in
`src/runtime/impl_vcs.rs` with real persistence for:

- `vcs_commit` — allocates a monotonic xid, pins it, writes the
  canonical `red_commits` row, advances the branch ref, updates
  the workset. Commit hash is
  `SHA-256("reddb-commit-v1" || root_xid || sorted_parents
  || author || message || timestamp_ms)`.
- `vcs_branch_create` / `_delete` / `_tag_create` / `_list_refs`
  / `_checkout` — CRUD over `red_refs` with short-name
  normalisation.
- `vcs_merge` — fast-forward path + non-FF merge commit +
  recursive 3-way JSON merge through
  `application::merge_json`. Non-FF merges populate
  `red_conflicts` with base/ours/theirs bodies so tooling can
  render a full 3-way diff without extra fetches.
- `vcs_cherry_pick` / `vcs_revert` — new commits on HEAD with
  prefixed messages; refuses root/merge sources. Records a
  `cp:<hex>` / `rv:<hex>` merge_state for Phase 6.2 data apply.
- `vcs_reset` — soft / mixed (move ref + workset). Hard is
  Phase 6.2.
- `vcs_log` — reverse topological walk from HEAD honouring
  `--from`, `--to`, `--limit`, `--skip`, `--no-merges`.
- `vcs_diff` — per-entity visibility diff at two MVCC xids;
  coalesces Add+Remove pairs sharing entity_id into `Modified`.
- `vcs_lca` — BFS from both sides, highest-height match.
- `vcs_resolve_commitish` / `vcs_resolve_as_of` — full hash,
  full ref, short branch/tag, short hash prefix (≥ 4 chars).

### SQL: `AS OF` time-travel clause

Parser extension in `src/storage/query/parser/table.rs`; new
`AsOfClause` on `core::TableQuery`. Runtime resolves to an MVCC
xid and installs a `CurrentSnapshotGuard` for the statement:

```sql
SELECT * FROM users AS OF COMMIT '<hash>' WHERE age > 21;
SELECT * FROM users AS OF BRANCH 'staging' LIMIT 5;
SELECT * FROM orders AS OF TAG 'v1.0' WHERE total > 100;
SELECT * FROM events AS OF TIMESTAMP 1710000000000;
SELECT * FROM t AS OF SNAPSHOT 42;
```

### REST: `/vcs/*` surface

14 endpoints wired into `src/server/routing.rs`:
`/vcs/commit`, `/vcs/branch`, `/vcs/branches`, `/vcs/branches/<name>`,
`/vcs/tag`, `/vcs/tags`, `/vcs/checkout`, `/vcs/merge`,
`/vcs/reset`, `/vcs/log`, `/vcs/diff`, `/vcs/status`, `/vcs/lca`,
`/vcs/conflicts/<merge_state_id>`.

### CLI: `red vcs <subcommand>`

Top-level `vcs` command with 11 subcommands —
`commit | branch | branches | tag | tags | checkout | merge |
reset | log | status | lca | resolve`. Honours `--json`,
`--path`, `--connection`, `--author`, `--email`, `--limit`,
`--branch`, `--from`, `-m/--message`, `--ff-only`, `--no-ff`.

### Standalone 3-way JSON merge (`application::merge_json`)

Pure library module with 15 unit tests covering disjoint edits,
array elementwise merges, length changes, deletion vs
modification, type mismatches, nested objects. No storage deps —
feeds cherry-pick / revert / merge conflict materialisation and
is reusable by other callers.

### Tests

- `tests/e2e_vcs.rs` — 13 cases: commit / branch / tag /
  checkout / log / tags / resolve_commitish / resolve_as_of /
  LCA / fast-forward / non-fast-forward / FF-only refusal /
  reset soft / reset hard stub / diff / status.
- `tests/e2e_vcs_phase5.rs` — 7 cases covering cherry-pick /
  revert / conflict count.
- `tests/e2e_vcs_as_of_parse.rs` — 8 cases covering every
  AS OF variant and regression guards for `SELECT col AS alias`.
- `tests/e2e_vcs_as_of_enforce.rs` — 6 cases covering runtime
  resolver + executor snapshot install.
- Unit tests: 15 `application::merge_json::tests` + 3 new
  `snapshot::tests::pin_*`.

Total: 2585 lib tests + 34 VCS e2e = 2619 green.

### Docs

- `docs/vcs/overview.md` — what + why + use cases
- `docs/vcs/architecture.md` — layers, hashing, merge algorithm
- `docs/vcs/commands.md` — exhaustive CLI / REST / SQL reference
- `docs/vcs/walkthrough.md` — tour of every subcommand
- `docs/guides/git-for-data.md` — end-to-end tutorial with real
  user data + conflict resolution
- `examples/vcs_showcase.sh` — runnable demo script

### Not in scope (Phase 6.2+)

- Worksets with real data staging (DML stamping during merge)
- `vcs_conflict_resolve` applying resolved JSON to user rows
- `reset --hard` selective MVCC rewind
- gRPC `VcsService` in `proto/reddb.proto`
- Remote push / pull (`red vcs push` / `pull`)

## 2026-04-22 — AI-first SQL surface + hypertable pipeline

Multi-sprint push to make the "AI-first multi-model" pitch
defensible from a user SQL session. Everything below is callable
without touching the Rust API — the engines (ML registry, semantic
cache, hypertable registry, continuous aggregate engine) now have
scalar-function entry points alongside the existing library code.

### AI / ML scalars (Sprint 1)

- `ML_CLASSIFY(model, features)` / `ML_PREDICT_PROBA(model, features)`
  — evaluate a registered classifier (logreg / naive bayes) against
  a feature vector or array. Returns class id or probability array.
- `MODEL_REGISTER(name, kind, weights_json [, hyperparams, metrics])`
  / `MODEL_DROP(name)` — lifecycle for pre-trained weights. Serving
  pipelines can ship JSON straight to production and activate via
  SQL.
- `EMBED(text [, provider])` — call the AI provider stack (OpenAI,
  Ollama, Groq, OpenRouter, Together, Venice, Deepseek, HuggingFace,
  or any OpenAI-compatible endpoint) to embed a text; returns
  `Vector`. `NULL` when provider / api-key not configured — fail-
  closed so a probe doesn't crash a query.
- `SEMANTIC_CACHE_GET(ns, embedding)` /
  `SEMANTIC_CACHE_PUT(ns, prompt, response, embedding)` — cosine-
  similarity cache for LLM responses. Shared `RedDB`-scoped instance
  so the cache is reachable from every session.
- `LIST_MODELS()` / `SHOW_MODELS()` — introspection.

Documented end-to-end in `docs/guides/rag-in-20-lines.md`.

### Hypertables (Sprint 2)

- `CREATE HYPERTABLE name TIME_COLUMN col CHUNK_INTERVAL 'dur'
   [TTL 'dur'] [RETENTION N DAYS]` — TimescaleDB-style DDL. Writes
  a Table-model contract and registers a `HypertableSpec`.
- `DROP HYPERTABLE name` — clears the registry entry and drops the
  backing collection.
- **INSERT-time chunk routing**: each row inserted into a hypertable
  is routed through `HypertableRegistry::route` so chunks allocate
  on demand and their bounds / row counts stay current without
  manual bookkeeping.
- `LIST_HYPERTABLES()` / `SHOW_HYPERTABLES()` — list registered names.
- `HYPERTABLE_PRUNE_CHUNKS(name, lo_ns, hi_ns)` — consult the
  partition pruner primitive over real allocated chunks. Returns
  the chunk names overlapping `[lo, hi)`. Exposes what the planner
  will consult before a scan.

### Continuous aggregates (Sprint 2)

Full end-to-end via scalars:

- `CA_REGISTER(name, source, bucket_dur, alias, agg, field [, lag,
   max_interval])` — single-column aggregate, any of
  `avg/min/max/sum/count/first/last`.
- `CA_REFRESH(name [, now_ns])` — scans the source collection for
  rows in the next safe window (bounded by `refresh_lag` and
  `max_interval_per_job`), folds them into bucket state.
- `CA_QUERY(name, bucket_start_ns, alias)` — reads the aggregated
  value from any bucket.
- `CA_STATE(name)` / `CA_LIST()` / `CA_DROP(name)`.

`CREATE CONTINUOUS AGGREGATE ... AS SELECT ... GROUP BY
time_bucket()` DDL form stays "planned" — the scalar surface
covers the hot path today.

### Schema

- `CREATE TABLE t(...) WITH (append_only = true)` now parses — the
  parenthesised form `WITH (k = v, k = v)` works everywhere the
  legacy `WITH k = v` shorthand does.

### Fixes

- `StoreCommitCoordinator::truncate` now resets both `wal.durable_lsn`
  **and** `WalAppendQueue.next_lsn` together. The previous mismatch
  hung post-checkpoint inserts — 16 `rpc_stdio` tests were
  `#[ignore]`-masked; all back on.

### Docs

- `docs/guides/rag-in-20-lines.md` — full RAG tutorial using the new
  scalars + MODEL_REGISTER path.
- `docs/data-models/overview.md` — plain-text "which model fits my
  use case?" decision tree.
- `docs/data-models/hypertables.md` — mental model, ASCII chunk
  fan-out, INSERT routing, DROP, and pruning surface.
- `docs/data-models/continuous-aggregates.md` — shipped SQL surface
  documented alongside the planned DDL form.
- `docs/README.md` — "When to reach for RedDB" table, honest about
  fits (RAG, multi-model, inline ML) and misfits (heavy OLAP,
  distributed sharding, PG-extension ecosystem).
- `docs/query/search-commands.md`, `docs/data-models/graphs.md`,
  `docs/query/graph-commands.md` — translate leftover
  Portuguese-language sections to English.

---

## 2026-04-22 — Performance & Stability

Bundle of perf work (04-20/04-21), dep bump, and one runtime hang fix.

### Wire / ingest

- **Streaming bulk wire protocol** — Postgres `COPY`-equivalent
  columnar stream over a persistent connection. ~3× `typed_insert`
  on 10k-row batches. See [Ingest API §6b](/api/ingest.md).
- **Columnar pre-validated insert path** — skips the N×ncols
  `String` clones the legacy wire bulk path paid.
- **Wire encode** — column indices are resolved once per result set
  and the output buffer is reused across rows.

### CDC

- **Split CDC lock** — concurrent CDC observers no longer serialise
  on a single mutex.

### WAL / durability

- **Lock-free append queue** for `WalDurableGrouped` mode. Writers
  enqueue pre-encoded blobs under one mutex; a group-commit
  coordinator drains in LSN order and fsyncs once per batch.
  Documented in [WAL & Recovery](/engine/wal.md).
- **Batched bulk inserts** — one WAL action per bulk op, not N.
- **Phase C busy-spin deadlock fix** under tokio preemption.
- **Truncate invariant** — the append queue's LSN cursor now
  resets alongside the WAL on checkpoint truncate. Fixes a hang
  where post-checkpoint inserts enqueued a target LSN in the old
  space the drain could never reach. All 16 previously-ignored
  `rpc_stdio` tests are back on.

### B-tree

- **Right-sibling hop** on sorted bulk insert — after filling a
  leaf the cursor advances via the sibling pointer instead of
  re-descending from the root. Hot path for the wire bulk protocol
  and the time-series chunk writer.

### Dependencies

- Rust 1.95.0 (toolchain)
- `tonic` 0.14 (split into `tonic-prost` + `tonic-prost-build`),
  `prost` 0.14
- `ureq` 3.3 with `rustls` feature (sync-only API); HTTP helpers
  consolidated
- `hmac` 0.13, `sha2` 0.11, `lz4_flex` 0.13, `roaring` 0.11,
  `rayon` 1.12, `rcgen` 0.14, `criterion` 0.8, `pprof` 0.15

---

## 2026-04-22 — TimescaleDB + ClickHouse Parity Push

Adds the foundations for competing directly with TimescaleDB (in
time-series / log workloads) and ClickHouse (in columnar OLAP).
Every item below ships as a library + unit tests; SQL parser wiring
for the DDL surfaces lands alongside the corresponding feature in a
follow-on sprint. See `/.claude/plans/eu-recebi-esta-review-serene-glade.md`
for the full plan.

### Time-series / logs

- **Hypertables** (`src/storage/timeseries/hypertable.rs`) — chunk
  auto-routing by timestamp, `show_chunks`, `drop_chunks_before`.
- **Continuous aggregates** (`src/storage/timeseries/continuous_aggregate.rs`) —
  incremental refresh driven by a `last_refreshed_bucket` watermark;
  respects `refresh_lag` + `max_interval_per_job`.
- **Retention daemon** (`src/storage/timeseries/retention.rs`) —
  cooperative sweeper, backend-agnostic trait, observable stats.
- **Partition TTL** (new) — `HypertableSpec::with_ttl("90d")` +
  `HypertableRegistry::sweep_expired` + per-chunk overrides +
  preview via `chunks_expiring_within`. Docs: `docs/data-models/partition-ttl.md`.
- **Log pipeline** (`src/storage/timeseries/log_pipeline.rs`) —
  `LogPipeline` bundles hypertable + retention + tail ring buffer;
  `LogLine` struct with severity + labels + numeric fields + trace
  IDs; `ingest_batch` / `tail_since` / `set_partition_ttl`.
- **Log docs** (new) — `docs/guides/using-reddb-for-logs.md` (full
  guide, ~400 lines) + `docs/guides/logs-quickstart.md`.

### Compression

- **T64 bit-packing** — integer codec for narrow-range columns.
- **zstd per-chunk fallback** with leading-marker passthrough for
  tiny inputs.
- **`select_int_codec` heuristic** — picks DeltaOfDelta / T64 / Raw
  by shape.
- **Per-column codec pipeline** (`src/storage/unified/segment_codec.rs`) —
  ColumnCodec enum (None / Lz4 / Zstd / Delta / DoubleDelta / Dict),
  chain header serialisable. Adds `lz4_flex` (pure-Rust) dep.

### Execution / OLAP

- **ColumnBatch + operators** (`src/storage/query/batch/`) — typed
  `ColumnVector` (Int64/Float64/Bool/Text + validity), filter /
  project / aggregate batch operators, 2048-row vectors.
- **SIMD reducers** (`src/storage/query/batch/simd.rs`) — AVX2
  sum_f64 / sum_i64 / min_f64 / max_f64 / filter_gt_f64 with scalar
  fallbacks, runtime-detected.
- **Parallel reducers** (`src/storage/query/batch/parallel.rs`) —
  rayon-driven `parallel_sum_f64` + `parallel_aggregate` with partial
  group-by merge.

### Planner primitives

- **Partition pruning** (`src/storage/query/planner/partition_pruning.rs`) —
  RANGE / LIST / HASH pruner with `PrunePredicate` AST, AND
  tightening + OR widening + FNV hash + conservative fallback.
- **Projections** (`src/storage/query/planner/projections.rs`) —
  ClickHouse-style pre-aggregation matcher; picks narrowest-fit
  projection; filter-signature compatibility check.

### Aggregates

- **T-Digest primitive** (`src/storage/primitives/tdigest.rs`) with
  merging variant + compact loop.
- **ClickHouse-parity aggregators** (`src/storage/query/engine/aggregates_extra.rs`) —
  Uniq (HLL-backed), QuantileTDigest, Covariance (Welford + parallel
  merge for corr / covar_pop / covar_samp), CountIf, SumAvgIf, Any,
  AnyLast, GroupArray, `arrayJoin` helper.

### Schema

- **APPEND ONLY tables** (`CREATE TABLE ... APPEND ONLY`) — catalog
  flag on `CollectionContract`; UPDATE / DELETE rejected at parse
  time before RLS. Docs: `docs/data-models/append-only-tables.md`.

### Docs

- New data-model pages: `hypertables.md`, `continuous-aggregates.md`,
  `append-only-tables.md`, `partition-ttl.md`.
- New architecture pages: `competitive-positioning.md`,
  `distributed-roadmap.md`.
- New engine page: `columnar-execution.md`.
- New guides: `using-reddb-for-logs.md`, `logs-quickstart.md`.
- Updated: `docs/_sidebar.md`, `docs/data-models/overview.md`,
  `docs/data-models/tables.md`, `docs/data-models/timeseries.md`,
  `docs/reference/limitations.md`, `README.md`.

## 2026-04-17 — Performance Parity Push (Phases 0-4 + P6.T1)

Reduces the measured gap vs. the reference row-store row-store on
`benches/bench_definitive_dual.py`. Spec + plan:
`docs/spec-performance-parity-2026-04-17.md`, `tasks/plan.md`.

### Phase 0 — Operational scaffolding

- `src/runtime/config_matrix.rs` — single source of truth for
  durability / concurrency / storage keys, with a two-tier contract:
  **Tier A** (critical) self-heals on boot (writes the default into
  `red_config` when absent); **Tier B** (optional) uses in-memory
  defaults, only appearing in `SHOW CONFIG` after a user write.
- `src/runtime/config_overlay.rs` — precedence env → file →
  `red_config` → default. `REDDB_<UP_DOTTED_KEY>` env vars are
  re-read every boot and never persist; `/etc/reddb/config.json`
  overlay is write-if-absent so a later `SET CONFIG` always wins.
- `Dockerfile` gains `/etc/reddb` volume + `REDDB_CONFIG_FILE`
  default. Single image, opinionated by default.
- `docs/engine/perf-bench.md` — reproduction guide + tuning surface.
- `SET CONFIG` / `SHOW CONFIG` now lowercase dotted keys so keyword
  segments (`MODE`, `SIZE`) don't mismatch the matrix.

### Phase 1 — Per-collection locking

- `src/runtime/locking.rs` — `Resource::{Global, Collection(name)}`
  + `LockerGuard` RAII wrapper over the pre-existing (but unused)
  `storage::transaction::lock::LockManager`. Ordered acquire +
  reverse-order release; illegal escalations return a typed error.
- Dispatch integrates a single mode-picker
  (`intent_lock_modes_for`):
  - Reads → `(Global, IS) → (Collection, IS)`
  - Writes → `(Global, IX) → (Collection, IX)`
  - DDL → `(Global, IX) → (Collection, X)`
  - Admin / control → no lock
- 4-test concurrent-writes suite proves 20 threads × 200 inserts
  across 5 collections complete without serialisation. DDL suite
  proves `CREATE TABLE`/`ALTER TABLE` take X mode and that a DDL on
  collection `a` doesn't block writers on `b`.

### Phase 2 — Durability default flipped

- `DurabilityMode::default()` swapped from `Strict` (per-commit
  fsync) to `WalDurableGrouped` (batched sync; writers wait for
  durability but fsyncs coalesce across concurrent commits).
  Matches PG's `synchronous_commit=on` throughput under load.
- `DurabilityMode::from_str` accepts the matrix spelling `"sync"` →
  `WalDurableGrouped` and `"strict"` → `Strict`.
- The group-commit coordinator, `GroupCommit` waiter, and
  `JournalFlusher`-style background thread were already implemented
  (`src/storage/wal/group_commit.rs`,
  `src/storage/unified/store/commit.rs`); this release wires the
  default and the matrix key.
- **Deferred:** true fire-and-forget async tier (P2.T4) — same
  group-commit primitives can drive it, spec stays open.

### Phase 3 — HOT decision helper + UPDATE fast path

- `src/storage/engine/hot_update.rs` — pure decision helper mirroring
  PG's `heap_update` (no indexed column modified + fits the page).
- `flush_applied_entity_mutation` consults the helper and skips the
  `index_entity_update` call when HOT fires — saves a registry-lock
  acquisition + damage-vector walk per UPDATE.
- Parser fix: `CREATE INDEX ... USING HASH` now accepts the `HASH`
  keyword token alongside the ident form.
- **Deferred:** page-local in-place rewrite + `t_ctid` chain walker
  (P3.T4) — requires new on-disk fields on entities; spec stays
  open.

### Phase 4 — Fused bulk-insert index maintenance

- `IndexStore::index_entity_insert_batch(collection, &rows)` — one
  registry-lock acquisition for the whole batch instead of N per
  row. Outer loop walks the index registry once; inner loop walks
  the batch. Mirrors PG's `heap_multi_insert` + `ExecInsertIndexTuples`.
- `MutationEngine::append_batch` routes through it.
- The upstream `bulk_insert` primitive + `create_rows_batch` wire
  path + gRPC `BulkInsertBinary` handler were already in place;
  this commit closes the index-fusion gap the plan identified.

### Phase 6.T1 — Background writer wired

- `PageCache::flush_some_dirty(max)` + `dirty_count()` — bounded
  snapshot of dirty pages plus count introspection.
- `Pager::flush_some_dirty(max)` / `dirty_fraction()` — mirror the
  bgwriter contract.
- `bgwriter::PagerDirtyFlusher` — production `DirtyPageFlusher`
  holding a `Weak<Pager>` so the background thread exits cleanly on
  database drop.
- `Database::open_with_config` spawns the bgwriter (non-read-only
  mode) with default config; `Database::bgwriter_stats()` accessor
  exposes rounds / pages_flushed / dirty fraction for tests + ops.

### Tests

79 new / extended integration tests across 15 suites (all green):

- `unit_locking` (5)  — compat matrix + 50-thread stress
- `unit_hot_update` (6) — pure decision coverage
- `e2e_locking_reads` (3) — SELECT acquires IS, disabled flag
  suppresses, admin doesn't lock
- `e2e_concurrent_writes` (4) — IX modes + 20 threads × 200 inserts
- `e2e_ddl_concurrency` (3) — X locks + DDL doesn't block other
  collections' writers
- `e2e_hot_update` (3) — unindexed / non-indexed-col / indexed-col
- `e2e_config_matrix` (7) — tier A self-heal, Tier B silence, env
  overrides, file overlay, durability mapping, idempotency

Plus every pre-existing integration suite (tenancy, within,
RLS-universal, cross-model MVCC, views, auto-index, multi-model)
stays green — zero regression across 80+ correctness tests.

### Commits

- `P0`: config matrix + overlay + Docker + perf-bench doc
- `P1`: `Arc<LockManager>` + `LockerGuard` + reads / writes / DDL wiring
- `P2`: `WalDurableGrouped` default
- `P3`: HOT helper + UPDATE fast path
- `P4`: fused secondary-index insert batch
- `P6.T1`: bgwriter `Weak<Pager>` wiring

### Deferred (tracked)

- **P2.T4** async commit tier — same group-commit primitives
- **P3.T4** t_ctid chain walker — needs new entity-format field
- **P5** full Lehman-Yao B-tree + STORE_VERSION_V8 migration —
  multi-day storage surgery; `next_leaf` right-link already present,
  `high_key` + lock-free descent + local-split lock pending

---

## 2026-04-17 — Structured Logging with `tracing` + File Rotation

Replaces ~140 ad-hoc `eprintln!` sites with a `tracing` /
`tracing-subscriber` / `tracing-appender` pipeline. Every server
event now carries levels, timestamps, and correlation fields
(`conn_id`, `tenant`, `transport`, `bind`, `peer`, `err`) that ops
tools can filter on.

### Features

- `reddb::telemetry::{init, TelemetryConfig, LogFormat,
  TelemetryGuard}` — façade over tracing-subscriber. Two layers:
  stderr (pretty / JSON) and optional daily-rotating file written
  via `tracing-appender`, both sharing the same `EnvFilter`.
- Background janitor (`telemetry::janitor::spawn`) purges rotated
  files older than `--log-keep-days` every hour. Silent no-op when
  no tokio runtime is active.
- `span::query_span`, `span::connection_span`, `span::listener_span`
  — pull `conn_id` / `tenant` out of the existing thread-locals and
  stamp them on every event inside the span.
- Server / gRPC / HTTP / wire / PG-wire / Unix-socket / MCP startup
  messages migrated to `tracing::info!` with structured `transport`
  + `bind` fields. Connection errors now emit `tracing::warn!` with
  `peer` and `err` fields.
- `auth::store` bootstrap and vault persist errors emit structured
  warnings so operators can grep for `target=reddb::auth` in JSON
  output.
- `execute_query` wrapped in a `query` span — every downstream log
  inherits `conn_id`, `tenant`, `query_len` automatically.

### CLI flags

Added to `red server`:

| Flag | Default | Purpose |
|------|---------|---------|
| `--log-dir <path>` | `<--path parent>/logs` | Directory for rotating log files |
| `--log-level <level>` | `info` | trace/debug/info/warn/error or `RUST_LOG` expression |
| `--log-format <fmt>` | `pretty` on TTY, `json` otherwise | `pretty` \| `json` |
| `--log-keep-days <N>` | `14` | Retention count for rotated files |
| `--no-log-file` | off | stderr-only mode |

`RUST_LOG` overrides `--log-level` when set.

### Embedded mode

The library never installs a subscriber itself. Embedders either
call `reddb::telemetry::init` to reuse the same config surface as
the server, or keep their own `tracing-subscriber` pipeline. Docs
at `docs/api/embedded.md#logging`.

### Dependencies

Adds `tracing = "0.1"`, `tracing-subscriber = "0.3"` (with
`env-filter`, `fmt`, `json`, `ansi`, `registry` features),
`tracing-appender = "0.2"`. ~80 KB stripped.

### Scope explicitly deferred

- Test-only `println!` / `eprintln!` inside `#[cfg(test)]` blocks
  (`storage/engine/algorithms/mod.rs`, `storage/primitives/*.rs`)
  left alone — they're expected benchmark output.
- CLI user-facing `println!` in `bin/red.rs` and `client/repl.rs`
  unchanged — that's command output on stdout, not operational
  logging.
- Metrics (Prometheus) and distributed tracing (OTEL) are separate
  plans, though `tracing` is the foundation for both.

Commits: [Phase 1 logging infra + Phase 2 server migration + Phase 3
storage noise + Phase 4 correlation spans + Phase 5 docs — bundled].

---

## 2026-04-17 — RedDB-Native Extensions (Cross-Model Feature Lift)

Takes every PG parity feature from the morning's bundle and extends
it across the other entity kinds (graph, vector, queue, timeseries)
so the RedDB idiom feels unified rather than table-centric.

### MVCC Universal — cross-model atomic transactions

Commit: [`7e80a60`](https://github.com/forattini-dev/reddb/commit/7e80a60)

- `stamp_xmin_if_in_txn` post-save hook on `RedDBRuntime` — stamps
  `xmin` on graph nodes / edges, vectors, documents, queue messages,
  and timeseries points as they leave the DevX builder API, without
  crossing the storage ↔ runtime layer boundary
- Visibility filter wired into every non-table scan:
  `graph_dsl::materialize_graph_with_projection`, queue
  `load_queue_message_views`, vector ANN (`db.similar()`)
  post-filter — all use the same `capture_current_snapshot()` /
  `entity_visible_with_context()` pair that tables use
- `delete_message_with_state` turns queue ACK into an MVCC
  tombstone when running inside a transaction (revived by
  `ROLLBACK`, flushed by `COMMIT`)
- `runtime::mvcc` pub re-export module so transports (PG wire,
  gRPC, HTTP) and integration tests can emulate per-connection
  isolation
- `detect_mode` now recognises `BEGIN` / `COMMIT` / `ROLLBACK` /
  `SAVEPOINT` / `RELEASE` / `VACUUM` / `ANALYZE` / `RESET` as
  bare-token SQL commands

**Impact:** one `BEGIN` spans tables + graphs + vectors + queues +
timeseries atomically. PG has no graphs/vectors. Neo4j has no
queues. Kafka has no MVCC. RedDB does all of it natively.

### ASK tenant-scoped — RLS in the RAG pipeline

Commit: [`c08cb3f`](https://github.com/forattini-dev/reddb/commit/c08cb3f)

- `search_entity_allowed(collection, entity, snap_ctx, rls_cache)` —
  per-entity gate wired into all three tiers of `search_context`
  (field-index, token-index, global scan)
- Restrictive default: RLS enabled + zero matching policy ⇒ deny.
  The LLM never reasons over rows the caller cannot see
- Acme session asking over a global corpus transparently filters to
  acme-tagged rows; globex gets its own view

### Tenancy dotted paths — JSON-native tenant discriminators

Commit: [`5a8ce89`](https://github.com/forattini-dev/reddb/commit/5a8ce89)

- `TENANT BY (root.sub.path)` on `CREATE TABLE` and
  `ALTER TABLE t ENABLE TENANCY ON (root.sub)` — arbitrary depth
- Root segment accepts keyword idents (`meta`, `data`, `tags`) that
  would otherwise tokenise as reserved words
- Evaluator parses `Value::Text` starting with `{` or `[` as JSON
  too — users who stash JSON in TEXT columns get the same
  filtering as declared JSON
- `merge_dotted_tenant` and `dotted_tail_already_set` helpers keep
  INSERT auto-fill idempotent: existing root JSON is merged
  in-place, missing root is synthesised as `{tail: tenant_id}`,
  explicit user values are trusted
- Result cache key now mixes tenant + auth identity — previously a
  `SELECT` filtered for acme would be served back to globex when
  the query string matched

### RLS universal per entity kind

Commit: [`55e928d`](https://github.com/forattini-dev/reddb/commit/55e928d)

- Grammar: `CREATE POLICY ... ON NODES|EDGES|VECTORS|MESSAGES|POINTS|DOCUMENTS OF <collection>`.
  The bare `ON <collection>` form still defaults to `TABLE` for
  backwards compatibility
- `PolicyTargetKind` stamped on every stored policy;
  `matching_rls_policies_for_kind` + `rls_policy_filter_for_kind`
  filter by kind when the scan path supplies one
- `runtime_any_record_from_entity` now materialises `QueueMessage`
  entities so policies like
  `USING (payload.tenant = CURRENT_TENANT())` can reach the JSON
  payload via the dotted-path resolver
- `evaluate_entity_filter_with_db` falls back to the any-record
  builder when the TableRow-only path returns `None` — non-tabular
  collections now actually evaluate policies instead of denying
  by default
- Queue scan calls `rls_policy_filter_for_kind(queue, Select, Messages)`
  — `ON MESSAGES OF` policies gate POP / LEN / PEEK; deny-default
  when RLS is on and no MESSAGES policy matches the role

### Views end-to-end + stacked filter merge

Commit: [`0353c6a`](https://github.com/forattini-dev/reddb/commit/0353c6a)

- Parser: accept both `Token::As` / `Token::Or` and their ident
  forms — `CREATE OR REPLACE VIEW … AS …` was erroring at the
  first token because the lexer promotes both to keyword tokens
- `execute_query` (raw SQL entry) now calls `rewrite_view_refs`
  before dispatch, mirroring `execute_query_expr`. Views
  previously only resolved through the prepared-statement path
- Stacked view rewrite: when a view body is itself a `TableQuery`,
  the outer query's `WHERE` (AND-combined), `LIMIT` (min), and
  `OFFSET` (added) are merged into the body instead of being
  silently dropped. `CREATE VIEW b AS SELECT * FROM a WHERE y;
  SELECT * FROM b WHERE z` now applies both predicates
- CREATE / DROP VIEW invalidate plan + result caches. `OR REPLACE`
  no longer serves stale rows from the obsolete body
- MATERIALIZED VIEW + REFRESH execute the body end-to-end

### Test coverage

Eleven end-to-end integration tests added, all green:

- `tests/e2e_cross_model_tx.rs` — 3 tests (graph / queue /
  multi-model atomic rollback)
- `tests/e2e_ask_tenant_scoped.rs` — 1 test (ASK corpus filtering)
- `tests/e2e_tenancy_dotted.rs` — 3 tests (reads, auto-fill
  missing root, merge existing JSON root)
- `tests/e2e_rls_universal.rs` — 1 test (MESSAGES-of-queue policy)
- `tests/e2e_views.rs` — 3 tests (filtered body, stacked, REFRESH)

### Housekeeping

- `DataType::Unknown` variant added — function catalog already
  referenced it for the polymorphic `JSON_SET(json, path, value)`
  third argument, blocking `cargo check`
- `match_if_exists` / `match_if_not_exists` widened to `pub(crate)`
  so the `sql.rs` parser router can reach them
- `src/storage/import/csv.rs` return-type mismatch (`.map(|_| ())`)

---

## 2026-04-17 — PostgreSQL Feature Parity (19 categories)

One bundled release closing every category on the PG parity matrix we
committed to. Each subsystem is shippable independently; grouped here
so the exhaustive-match refactors stay atomic.

Commit: [`81a6155`](https://github.com/forattini-dev/reddb/commit/81a6155)

### Transactions & Visibility (MVCC)

- `BEGIN` / `COMMIT` / `ROLLBACK` with per-connection `TxnContext`
- `SAVEPOINT` / `RELEASE SAVEPOINT` / `ROLLBACK TO SAVEPOINT` with
  sub-xid stack (nested rollback without aborting the parent txn)
- Tuple-level `xmin` / `xmax` stamping on INSERT + DELETE
- Read-path visibility filter wired across ~15 scan sites (sequential,
  parallel, index-assisted, bloom, bitmap-AND, fast-path entity lookup,
  aggregates, vector similarity, universal scan)
- Own-xid visibility: writer always sees own writes from sub-transactions
- DELETE inside a transaction becomes an MVCC tombstone (`xmax` stamped,
  physical removal deferred to COMMIT); ROLLBACK revives the tuple
- Autocommit snapshot uses `peek_next_xid()` so committed rows stay
  visible (previously 0 broke MVCC reads)
- Docs: [Transactions](query/transactions.md)

### Security

- `CREATE POLICY` / `DROP POLICY` — row-level security with a USING
  predicate per `(table, role, action)`
- `ALTER TABLE ENABLE / DISABLE ROW LEVEL SECURITY`
- RLS gates in SELECT, UPDATE, DELETE (OR-combined across policies,
  AND-folded into the user's WHERE)
- mTLS certificate authentication — `CommonName` + `SAN rfc822Name`
  modes with OID-to-role mapping
- OAuth / OIDC bearer-token validator — pluggable JWT verifier closure,
  standard `iss` / `aud` / `exp` / `nbf` claims
- Docs: [RLS](security/rls.md), [mTLS & OAuth](security/overview.md)

### Multi-Tenancy

- Session handle: `SET TENANT 'id'` / `RESET TENANT` / `SHOW TENANT`
- Scalar functions: `CURRENT_TENANT()`, `CURRENT_USER()`,
  `SESSION_USER()`, `CURRENT_ROLE()`
- Declarative: `CREATE TABLE t (...) TENANT BY (col)` — auto-RLS policy
  + INSERT auto-fill of the tenant column
- Retrofit: `ALTER TABLE t ENABLE TENANCY ON (col)` /
  `ALTER TABLE t DISABLE TENANCY`
- Persistence: tenant-table markers replayed from `red_config` on boot
- Docs: [Multi-Tenancy](security/multi-tenancy.md)

### Views & Materialized Views

- `CREATE VIEW` / `CREATE OR REPLACE VIEW` / `DROP VIEW`
- `CREATE MATERIALIZED VIEW` / `REFRESH MATERIALIZED VIEW` with
  `REFRESH POLICY` (manual / on-write / interval)
- Query rewriter descends `TableSource::Subquery` recursively so
  `SELECT ... FROM view_of_view` works
- Docs: [Views](query/views.md)

### Partitioning

- `CREATE TABLE ... PARTITION BY RANGE|LIST|HASH (col)`
- `ALTER TABLE parent ATTACH PARTITION child FOR VALUES ...`
- `ALTER TABLE parent DETACH PARTITION child`
- Registry-only in Phase 2.2; planner-level partition pruning in
  Phase 4
- Docs: [Partitioning](query/partitioning.md)

### Replication

- `QuorumCoordinator` with `Async` / `Sync` / `Regions(N)` modes
- Multi-region replica binding + LSN-watermark consistent reads
- Quorum-ack write path over existing replication infra

### Backup & Recovery

- `red dump -c <collection> -o file.rdbdump`
- `red restore -i file.rdbdump`
- Point-in-time recovery: `red pitr-list`, `red pitr-restore
  --target-time '2026-04-17 12:30:00 UTC'`

### Foreign Data Wrappers

- `ForeignDataWrapper` trait + `ForeignTableRegistry`
- Built-in CSV wrapper (RFC 4180 inline parser, per-table options)
- DDL: `CREATE SERVER` / `CREATE FOREIGN TABLE` / `DROP SERVER` /
  `DROP FOREIGN TABLE`
- Read-path intercept: foreign-table scans bypass native collection
  lookup
- Docs: [Foreign Data Wrappers](guides/foreign-data-wrappers.md)

### Network

- PostgreSQL v3 wire protocol — startup negotiation, simple query
  (`Q` / `T` / `D` / `C` / `Z` frames), SSL rejection with `N`
- PG type OID mapping (~20 variants)
- Unix socket transport via `tokio::net::UnixListener`
  (`--wire-bind unix:/tmp/reddb.sock`)
- Docs: [PostgreSQL Wire Protocol](api/postgres-wire.md)

### DDL & Maintenance

- `VACUUM [FULL] [table]` — triggers GC + page flush + stats refresh
- `ANALYZE [table]` — refreshes planner statistics
- `CREATE SCHEMA [IF NOT EXISTS] name` / `DROP SCHEMA`
- `CREATE SEQUENCE name START ... INCREMENT ...` + `nextval()` /
  `currval()` scalars
- Docs: [Maintenance & DDL Extras](query/maintenance.md)

### JSON & Imports

- `json_extract(json, '$.path')` / `json_set(json, '$.path', value)` /
  `json_array_length(json)` / `json_path_query(json, path)`
- `COPY table FROM 'file.csv' [WITH (DELIMITER ',', HEADER true)]`
- CSV import path paralleling JSONL / Parquet (bulk insert)

### Platforms

- Windows + macOS CI matrix (`cargo check --locked --all` +
  `cargo test --locked --lib`)
- `#[cfg(windows)]` gates for systemd, mmap, `/proc`, Unix socket
  (gracefully skipped)
- `null_device()` helper returning `NUL` on Windows and `/dev/null`
  elsewhere

---

## 2026-04-16 — WAL Durability & BRIN Timeseries

### WAL Durability

Commit: [`c63ed06`](https://github.com/forattini-dev/reddb/commit/c63ed06)

- WAL-backed writes: INSERT / UPDATE / DELETE hit the log before the
  pager, making every committed mutation crash-safe
- Configurable fsync policy (`every_commit` / `every_n` / `interval`)
- Boot-time replay of unflushed WAL frames
- Integration tests covering crash-and-recover flows

### BRIN Timeseries Indexing

Commit: [`eeb5dd5`](https://github.com/forattini-dev/reddb/commit/eeb5dd5)
+ correctness fix [`d6f6d9c`](https://github.com/forattini-dev/reddb/commit/d6f6d9c)

- Block-range zone maps for time-series segments (BRIN-equivalent)
- Min/max per block + binary search pruning
- Race-free reads under concurrent writers (lock contention fixed)
- O(log n) range scans on timestamp-ordered metric streams

### Storage Correctness

Commits: [`f98cc61`](https://github.com/forattini-dev/reddb/commit/f98cc61),
[`4eb4f77`](https://github.com/forattini-dev/reddb/commit/4eb4f77),
[`6a5ce02`](https://github.com/forattini-dev/reddb/commit/6a5ce02),
[`cb08fe3`](https://github.com/forattini-dev/reddb/commit/cb08fe3),
[`820274f`](https://github.com/forattini-dev/reddb/commit/820274f),
[`8e24f31`](https://github.com/forattini-dev/reddb/commit/8e24f31)

- Metadata persistence in binary file format (bumped to
  `STORE_VERSION_V7`)
- Race-free sealed-segment deletes + table-scoped result cache
- Arc-wrapped prepared statements — no more per-call re-plan
- Centralised sealed→growing rewrite path; UPDATE persisted via pager
- CDC + context index gated on confirmed deletion (eliminated
  double-emit)
- Canonical sealed-segment mutations — durable bulk persist path
- 4 critical bugs fixed: sealed mutations, delete ordering, unsafe flag
  rename, spill codec type mismatches

### Query Performance

Commit: [`9818697`](https://github.com/forattini-dev/reddb/commit/9818697)

- Unified mutation engine: single kernel for single-row / micro-batch /
  bulk INSERT
- Slot-indexed aggregates
- Covered queries: skip heap fetch when projection ⊆ indexed column
- Prepared statement Arc reuse across calls

---

## Earlier — New Data Structures & Query Extensions

### New Data Structures (10 total)

- **Bloom Filter**: Per-segment probabilistic key testing,
  auto-populated on insert, bloom pruning in query executor
- **Hash Index**: O(1) exact-match lookups via
  `CREATE INDEX ... USING HASH`
- **Bitmap Index**: Roaring bitmap for low-cardinality columns via
  `CREATE INDEX ... USING BITMAP`
- **R-Tree Spatial Index**: Radius/bbox/nearest-K geo queries via
  `SEARCH SPATIAL` and `CREATE INDEX ... USING RTREE`
- **Skip List + Memtable**: Write buffer in GrowingSegment, sorted
  drain on seal
- **HyperLogLog**: Approximate distinct counting (`CREATE HLL`,
  `HLL ADD/COUNT/MERGE`)
- **Count-Min Sketch**: Frequency estimation (`CREATE SKETCH`,
  `SKETCH ADD/COUNT`)
- **Cuckoo Filter**: Membership testing with deletion (`CREATE FILTER`,
  `FILTER ADD/CHECK/DELETE`)
- **Time-Series**: Chunked storage with delta-of-delta timestamps,
  Gorilla XOR compression, retention policies, time-bucket aggregation
- **Queue / Deque**: FIFO/LIFO/Priority message queue with consumer
  groups (`CREATE QUEUE`, `QUEUE PUSH/POP/PEEK/LEN/PURGE/ACK/NACK`)

### Query Language Extensions

- `CREATE INDEX [UNIQUE] name ON table (cols) USING HASH|BTREE|BITMAP|RTREE`
- `DROP INDEX [IF EXISTS] name ON table`
- `SEARCH SPATIAL RADIUS lat lon km COLLECTION col COLUMN col [LIMIT n]`
- `SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col`
- `SEARCH SPATIAL NEAREST lat lon K n COLLECTION col COLUMN col`
- `CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]`
- `DROP TIMESERIES [IF EXISTS] name`
- `CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]`
- `DROP QUEUE [IF EXISTS] name`
- `QUEUE PUSH|POP|PEEK|LEN|PURGE|GROUP CREATE|READ|ACK|NACK`
- `CREATE/DROP HLL|SKETCH|FILTER` + `HLL ADD/COUNT/MERGE` +
  `SKETCH ADD/COUNT` + `FILTER ADD/CHECK/DELETE`
- JSON inline literals: `{key: value}` without quotes in VALUES and
  QUEUE PUSH

### Deep Integration

- **IndexStore**: Unified manager for Hash/Bitmap/Spatial indices in
  RuntimeInner
- **IndexSelectionPass**: Query optimizer analyzes WHERE and recommends
  Hash/BTree/Bitmap automatically
- **Bloom filter pruning**: Executor skips segments when bloom says key
  is absent
- **Spatial search**: Functional radius/bbox/nearest with Haversine
  distance on GeoPoint and lat/lon fields
- **Memtable**: Write buffer integrated in GrowingSegment lifecycle
- **red_config**: All new features configurable via
  `SET CONFIG red.indexes.*`, `red.memtable.*`, `red.probabilistic.*`,
  `red.timeseries.*`, `red.queue.*`
- **ProbabilisticCommand dispatch**: HLL/CMS/Cuckoo fully functional
  end-to-end via SQL

### Dependencies

- Added `roaring = "0.10"` (Bitmap Index)
- Added `rstar = "0.12"` (R-Tree Spatial Index)

### Earlier

- Initial public release preparation.
- Added multi-model embedded/server/serverless documentation and
  packaging pipeline.
- Added unified crate publishing workflow for crates.io.
