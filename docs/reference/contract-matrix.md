# Public Contract Matrix

Source of truth for the behaviors RedDB promises in public docs, README, drivers,
and SDK helpers. Each row links a promise to its proof (a passing test) or
disclaims it (open issue, "Unsupported / planned").

Status vocabulary:

- **Proven** — at least one passing test in this repo exercises the documented
  syntax and result shape.
- **Partial** — basic path is covered; specific sub-promise (option, edge case,
  transport parity) is not.
- **Unsupported / planned** — documented somewhere but not implemented or not
  test-pinned; link to the tracking issue.

Companion docs:

- [`docs/conformance/public-surface-contract-matrix.md`](../conformance/public-surface-contract-matrix.md)
  — the feedback-driven ledger (PSC-xxx rows) tied to `feedbacks.md` /
  `feedbacks-new.md` scenarios.
- [`docs/reference/limitations.md`](limitations.md) — high-level shipped vs.
  planned table.

## SQL commands

| Promise | Doc | Proof / Status |
|---|---|---|
| `INSERT INTO ... RETURNING *` row envelope (RID + columns) | [docs/query/insert.md](../query/insert.md) | Proven — `tests/e2e_rid_row_envelope.rs`, `tests/e2e_returning.rs` |
| Document INSERT with quoted JSON literal (`'{...}'`) | [docs/data-models/documents.md](../data-models/documents.md) | Proven — `tests/e2e_documents_first_class_crud.rs` |
| `KV PUT 'key' = 'val'` and `PUT CONFIG ns key = val` | [docs/data-models/key-value.md](../data-models/key-value.md) | Proven — `tests/e2e_kv_namespaced_keys.rs`, `tests/e2e_config_crud.rs` |
| `CREATE HYPERTABLE name TIME_COLUMN ts CHUNK_INTERVAL '1d' [TTL '7d']` | [docs/data-models/hypertables.md](../data-models/hypertables.md) | Proven — `tests/e2e_create_hypertable.rs`, `tests/e2e_hypertable_retention.rs`, `tests/e2e_hypertable_prune.rs` |
| `CREATE HYPERTABLE name (col list...) WITH (ttl=...)` column-list form | [docs/data-models/timeseries.md](../data-models/timeseries.md) | Unsupported / planned — flagged in `docs/guides/logs-quickstart.md`, refs [#465](https://github.com/reddb-io/reddb/issues/465) |
| `UPDATE` / `DELETE` row-target conformance pack | [docs/query/update.md](../query/update.md), [docs/query/delete.md](../query/delete.md) | Proven — `tests/e2e_update_conformance_pack.rs`, `tests/e2e_explicit_update_targets.rs` |
| `SAVEPOINT` / `RELEASE` / `ROLLBACK TO` | [docs/query/transactions.md](../query/transactions.md) | Proven — `tests/e2e_savepoints.rs`, `tests/e2e_savepoint_update_reversal.rs` |
| MVCC isolation (first-committer-wins on conflicting writes) | [docs/architecture/mvcc-read-resolver.md](../architecture/mvcc-read-resolver.md) | Proven — `tests/e2e_isolation_levels.rs`, `tests/e2e_mvcc_first_committer_wins.rs` |
| `EXPLAIN` plan output | [docs/query/select.md](../query/select.md) | Proven — `tests/e2e_explain.rs` |
| `CREATE VIEW` / `CREATE CONTINUOUS AGGREGATE` | [docs/query/views.md](../query/views.md) | Proven — `tests/e2e_views.rs`, `tests/e2e_continuous_aggregate.rs` |
| `WITHIN` clause for spatial filters | [docs/query/spatial-search.md](../query/spatial-search.md) | Proven — `tests/e2e_within_clause.rs`, `tests/e2e_within_multi_model.rs` |

## Graph commands

| Promise | Doc | Proof / Status |
|---|---|---|
| `MATCH` node/edge/label/property projection + LIMIT | [docs/query/graph-commands.md](../query/graph-commands.md) | Partial — `tests/e2e_graph_public_envelope.rs`, `tests/e2e_feedback_regression_pack.rs`. Edge-property projection + case rules still flagged in PSC-002. |
| `GRAPH CENTRALITY ALGORITHM pagerank` | [docs/query/graph-commands.md](../query/graph-commands.md) | Partial — runtime path exercised via `tests/e2e_graph_public_envelope.rs`. Transport parity tracked in PSC-003. |
| `GRAPH NEIGHBORHOOD`, `SHORTEST_PATH`, `TRAVERSE` | [docs/query/graph-commands.md](../query/graph-commands.md) | Partial — basic envelopes covered by `tests/e2e_graph_public_envelope.rs`; label-edge filtering + label IDs flagged PSC-003, PSC-NEW-02. |
| `GRAPH CLUSTERING`, `TOPOLOGICAL_SORT`, `PROPERTIES`, `PATH FROM` | [docs/query/graph-commands.md](../query/graph-commands.md) | Unsupported / planned — parser accepts the syntax (`graph_commands.rs:25-33`) but no e2e test pins behavior. Refs [#465](https://github.com/reddb-io/reddb/issues/465) iter 2 note. |
| `GRAPH HITS` | n/a | Unsupported / planned — removed from `docs/query/graph-commands.md` in #465 iter 2; parser does not list HITS subcommand. |

## HTTP endpoints

| Endpoint | Doc | Proof / Status |
|---|---|---|
| `POST /query` (HTTP query parity with embedded) | [docs/api/http.md](../api/http.md) | Proven — `crates/reddb-server/tests/conformance.rs`, `crates/reddb-server/tests/runtime_query_behavior.rs` |
| `POST /ai/*` transport surface | [docs/api/http.md](../api/http.md) | Proven — `crates/reddb-server/tests/ai_transport.rs` |
| `GET /backup/status`, `POST /backup/start`, `POST /backup/restore` | [docs/api/http.md](../api/http.md) | Proven — handlers in `crates/reddb-server/src/server/handlers_backup.rs`; recovery covered by `tests/e2e_backup_restore.rs`. Endpoint-level smoke tracked in [#517](https://github.com/reddb-io/reddb/issues/517). |
| Prometheus `/metrics` exposition format | [docs/reference/metrics.md](metrics.md) | Proven — `tests/e2e_metrics_prometheus_*.rs` (histogram, counter, query, query_range, aggregation), `tests/e2e_metrics_grafana_compat_smoke.rs` |
| Prometheus remote-write ingest | [docs/api/ingest.md](../api/ingest.md) | Proven — `tests/e2e_metrics_remote_write.rs` |
| Tenant-isolated metrics | [docs/reference/metrics.md](metrics.md) | Proven — `tests/e2e_metrics_tenant_isolation.rs` |
| Readiness / bind-collision behavior | [docs/reference/health.md](health.md) | Unsupported / planned — PSC-007, FB-NEW-30. No automated bind-collision test yet. |

## SDK helpers (per language)

Helper surface defined in [`docs/clients/sdk-helper-spec.md`](../clients/sdk-helper-spec.md);
shipped via [#463](https://github.com/reddb-io/reddb/issues/463) and follow-up READMEs swept in #465 iter 3.

| Surface (Documents / KV / Queue) | Languages | Proof / Status |
|---|---|---|
| Rust reference helpers | rust | Proven — `crates/reddb-client/README.md`, `crates/reddb-client/tests/` |
| Go helpers (`NewHelpers`, Documents/KV/Queue) | go | Proven — `drivers/go/helpers_test.go` |
| Dart helpers + typed errors (`InvalidArgument`, `NotFound`, `InvalidResponse`) | dart | Proven — `drivers/dart/test/helpers_test.dart` |
| Java helpers + `HelperException.{InvalidArgument,NotFound,InvalidResponse}` | java | Proven — `drivers/java/src/test/java/dev/reddb/helpers/HelpersTest.java` |
| .NET helpers (`KvClient.ListOpts`, `QueueClient.PushOptions`) | dotnet | Proven — `drivers/dotnet/tests/Reddb.Tests/HelpersTests.cs` |
| PHP helpers + `Reddb\Helpers\{InvalidArgument,NotFound,InvalidResponse}` | php | Proven — `drivers/php/tests/Helpers/HelpersTest.php` |
| JS / JS-client `documents` / `kv` / `queue` namespaces, `RedDBError` codes | js, js-client | Proven — `drivers/js/test/`, `drivers/js-client/test/` (see driver READMEs) |
| Python sync + asyncio helpers | python, python-asyncio | Proven — pyo3 binding + `drivers/python-asyncio/README.md` (SEARCH SIMILAR example aligned with parser in #465 iter 3) |
| C++ / Kotlin / Zig helpers | cpp, kotlin, zig | Proven — landed fresh in [#463](https://github.com/reddb-io/reddb/issues/463) iter 3; READMEs under `drivers/{cpp,kotlin,zig}/README.md` |
| Parameterized query contract (`$1` / `params=`) across all drivers | all | Proven — ADR 0015, `crates/reddb-server/tests/parser_hardening.rs`, `crates/reddb-server/tests/sql_injection_audit.rs` |

## Probabilistic structures

| Promise | Doc | Proof / Status |
|---|---|---|
| HLL approximate cardinality (write + read) | [docs/query/probabilistic-commands.md](../query/probabilistic-commands.md) | Proven — `tests/e2e_probabilistic_public_contract.rs`, `tests/e2e_feedback_regression_pack.rs` |
| Count-Min SKETCH frequency estimate | [docs/query/probabilistic-commands.md](../query/probabilistic-commands.md) | Proven — `tests/e2e_probabilistic_public_contract.rs` |
| Bloom FILTER membership test | [docs/query/probabilistic-commands.md](../query/probabilistic-commands.md) | Proven — `tests/e2e_probabilistic_public_contract.rs` |
| HLL / SKETCH / FILTER parameter options | [docs/query/probabilistic-commands.md](../query/probabilistic-commands.md) | Unsupported / planned — PSC-012, FB-OLD-21. Parser accepts DDL; options not honored yet. |
| Parser snapshots for probabilistic command grammar | n/a | Proven — `crates/reddb-server/tests/probabilistic_parser.rs`, `probabilistic_parser_snapshots.rs` |

## ASK / SEARCH

| Promise | Doc | Proof / Status |
|---|---|---|
| `SEARCH SIMILAR $1 COLLECTION col LIMIT n` (vector ANN) | [docs/query/search-commands.md](../query/search-commands.md) | Proven — `crates/reddb-server/tests/vector_search_parser.rs`, `vector_search_snapshots.rs` |
| `SEARCH CONTEXT` bucket coverage + tenant scoping | [docs/guides/ask-your-database.md](../guides/ask-your-database.md) | Proven — `tests/e2e_ask_search_conformance.rs`, `tests/e2e_ask_tenant_scoped.rs`, `crates/reddb-server/tests/ask_parser.rs`, `ask_snapshots.rs` |
| ASK grounding + citations contract | [ADR 0013](../adr/0013-ask-grounding-citations.md) | Proven — `tests/e2e_ask_search_conformance.rs`, closed via [#464](https://github.com/reddb-io/reddb/issues/464) |
| `SEARCH IVF` SQL form | n/a | Unsupported / planned — removed from `docs/query/search-commands.md` in #465 iter 2; runtime auto-picks IVF when present. |

## Data models — multi-model durability

| Promise | Doc | Proof / Status |
|---|---|---|
| Cross-model transactions (tables + graph + KV in one tx) | [docs/data-models/overview.md](../data-models/overview.md) | Proven — `tests/e2e_cross_model_tx.rs`, `tests/e2e_multimodel_flow.rs` |
| Backup → restore round-trip | [docs/api/http.md](../api/http.md) | Proven — `tests/e2e_backup_restore.rs` |
| WAL crash recovery (DWB fold OFF + ON) | [docs/engine/wal.md](../engine/wal.md) | Proven — `tests/e2e_fold_dwb_into_wal_crash.rs`, `tests/e2e_logical_wal_crash.rs` (closed [#478](https://github.com/reddb-io/reddb/issues/478)) |
| Append-only / events tables (CDC + backfill) | [docs/query/events.md](../query/events.md) | Proven — `tests/e2e_append_only.rs`, `tests/e2e_events_foundation.rs`, `tests/e2e_events_cdc_rid.rs`, `tests/e2e_events_backfill.rs` |
| File-backed rebuild order independence | [docs/engine/architecture.md](../engine/architecture.md) | Unsupported / planned — PSC-016, FB-NEW-10/31. Persistence ordering regression not yet landed. |
| Red collections acceptance (schema + RID envelope) | [docs/reference/red-schema.md](red-schema.md) | Proven — `tests/e2e_red_collections_acceptance.rs`, `tests/e2e_red_schema.rs` |

## Maintenance notes

- A row promoted to **Proven** must point at a real file on `main`. When a test
  is renamed or removed, the matching row must be updated in the same PR.
- A row moved to **Unsupported / planned** must cite an issue number. If no
  issue exists, open one before flipping the status.
- Driver-helper rows track the helper surface only. Wire-level smoke (frame,
  scram, redwire conn, value codec) is covered by per-driver `*_test.*` peers
  in the same directory and is not enumerated here.
- This matrix is representative, not exhaustive — see
  `docs/conformance/public-surface-contract-matrix.md` for the full
  PSC-xxx ledger tied to feedback files.
