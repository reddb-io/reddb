# RedDB Dogfood UX + Quality Improvement Plan

Goal: turn the first-run RedDB experience into a polished, trustworthy path for embedded, server, HTTP, and future hosted SaaS usage.

Architecture: attack issues from outside-in. First stabilize public contracts and first-run operator signals, then fix model-specific UX, then improve docs/demos/SDK ergonomics. Every behavior change should start with a regression test that reproduces the frustration.

Principles:
- Test first for every bug fix.
- Prefer surgical changes over broad refactors.
- Keep public behavior explicit and documented.
- Separate internal engine metadata from user-facing API shape.
- Make fresh database startup feel healthy and safe.

---

## Phase 0: Baseline and acceptance harness

### Task 0.1: Add a dogfood smoke script or test fixture

Objective: preserve the exact first-run path used during dogfood so regressions are caught.

Candidate coverage:
- fresh persistent DB
- `SELECT 1`
- create table, insert, select
- insert document, select projected fields
- KV put/get
- HLL/sketch/filter basic operations
- hypertable create/insert/select
- graph node/edge/path happy path
- queue create/push/pop
- HTTP `/query` equivalent for a subset

Likely files:
- Add test under `tests/e2e_dogfood_first_run.rs` or extend existing feedback regression tests.
- Possibly add shell script under `scripts/dogfood-smoke.sh` later, but automated Rust test first.

Verification:
- `cargo test --locked --test e2e_dogfood_first_run -- --nocapture`
- `cargo test --locked --test e2e_feedback_regression_pack -- --nocapture`

---

## Phase 1: Fresh server health must not look broken

Problem: a brand-new server returned HTTP 503 `/health` with `state: degraded`, while `/query` worked. This damages trust immediately, especially for SaaS/operators.

### Task 1.1: Reproduce degraded fresh health in a test

Objective: create a failing test that starts a fresh persistent HTTP server and asserts readiness semantics.

Expected public behavior to decide:
- Liveness endpoint should be 200 if process is alive.
- Readiness endpoint should be 200 once queries/writes are available.
- Deep diagnostics can report internal index/catalog drift without making simple health fail, unless it affects user operations.

Likely files:
- `tests/http_health_fresh_db.rs` or existing HTTP health test file.
- Server health handlers likely under `crates/reddb-server/src/server/handlers_*` and/or health/doctor modules.

Test cases:
1. Fresh DB + server starts.
2. `POST /query SELECT 1` succeeds.
3. Health/readiness endpoint returns non-degraded user-facing status, or a separate endpoint clearly reports diagnostic-only warnings.

Verification:
- targeted health test fails before fix, passes after.

### Task 1.2: Split liveness, readiness, and deep diagnostics semantics

Objective: make operator signals unambiguous.

Suggested contract:
- `/health` or `/admin/health`: lightweight liveness, 200 while server can answer.
- `/ready`: query/write readiness, 200 if public operations work.
- `/doctor` or `/admin/doctor`: detailed diagnostic status; may return degraded with internal catalog drift details.

If changing routes is too large, preserve existing routes but change status classification so fresh DB is not degraded when public operations work.

Verification:
- fresh server health test.
- existing doctor/health tests.
- manual curl: `GET /health`, `POST /query {"query":"SELECT 1"}`.

---

## Phase 2: Graph happy path and contract cleanup

Problem: inserting graph nodes by label worked; inserting edge by `from`/`to` labels unexpectedly failed in dogfood, while docs/tests imply label-based flows in places. RIDs worked. Shortest path with labels returned a row with null path fields, which was ambiguous.

### Task 2.1: Decide and document graph edge identity contract

Objective: choose official API behavior.

Recommendation:
- Support both human label form and explicit RID form.
- `from`/`to` resolve node labels within the graph collection.
- `from_rid`/`to_rid` use internal IDs.
- Ambiguous duplicate labels should return a clear error asking for RID.

Docs to update after implementation:
- `docs/query/graph-commands.md`
- `docs/data-models/graphs.md`
- `docs/reference/sql-1-0-x.md`
- README examples if needed.

### Task 2.2: Add regression test for label-based edge insert

Objective: reproduce dogfood failure.

Test flow:
- `INSERT INTO social NODE (label, name) VALUES ('alice', 'Alice')`
- `INSERT INTO social NODE (label, name) VALUES ('bob', 'Bob')`
- `INSERT INTO social EDGE (label, from, to, evidence) VALUES ('FOLLOWS', 'alice', 'bob', 'demo') RETURNING *`
- `MATCH ... RETURN a.name, b.name, r.evidence` returns Alice/Bob/demo.

Likely files:
- Existing graph tests: `tests/http_query_grimms_graph.rs`, `tests/e2e_issue_544_graph_insert_returns_labels_ids.rs`, `tests/e2e_issue_553_graph_edge_property_projection.rs`, `tests/e2e_issue_556_graph_sql_http_parity_and_limits.rs`.
- Parser/runtime likely under `crates/reddb-server/src/storage/query/parser/graph_commands.rs`, `crates/reddb-server/src/runtime/impl_graph_commands.rs`, DML insertion paths.

Verification:
- targeted graph test fails before fix, passes after.

### Task 2.3: Make no-path shortest path explicit

Objective: avoid returning a success-looking row with null path semantics.

Recommended behavior:
- Either return zero records for no path; or
- return one record with `path_found=false`, `hop_count=NULL`, plus clear source/target.

Need decision because this is public API compatibility-sensitive.

Verification:
- test no-edge graph: two nodes, no edge, shortest path returns explicit no-path shape.
- existing graph tests still pass.

---

## Phase 3: HTTP query response shape polish

Problem: `SELECT name, price FROM products` returned `columns: ["name", "price"]`, but each record's `values` included internal metadata fields (`rid`, `collection`, `created_at`, `kind`, etc.). That is surprising for SQL clients and SDKs.

### Task 3.1: Define user-facing result envelope contract

Objective: decide where metadata belongs.

Recommendation:
```json
{
  "result": {
    "columns": ["name", "price"],
    "records": [
      {
        "values": {"name":"Red Cloud", "price":99},
        "meta": {"rid":103, "collection":"products", "kind":"row", "created_at":...}
      }
    ]
  }
}
```

Compatibility option:
- Keep old behavior behind `include_metadata=true` or `?include_metadata=true`.
- Or version the endpoint later (`/v1/query`) and keep compatibility on old path.

### Task 3.2: Add projection test for HTTP SQL response

Objective: prove selected columns produce selected values only, unless metadata is requested.

Test flow:
- create table products
- insert rows
- query `SELECT name, price FROM products ORDER BY id ASC`
- assert `result.columns == ["name", "price"]`
- assert each row values contains only `name` and `price`
- assert metadata is absent or nested in `meta` depending on chosen contract.

Likely files:
- `crates/reddb-server/src/server/handlers_query.rs`
- runtime query result serialization paths
- existing HTTP query tests.

Verification:
- targeted HTTP test.
- existing HTTP tests.

---

## Phase 4: Document-store reserved fields and “save anything” ergonomics

Problem: top-level document field `kind` is rejected as reserved. Correct from engine perspective, but common real-world JSON uses `kind`, `type`, `id`, `tenant`, etc. This conflicts with “save anything”.

### Task 4.1: Publish reserved-field contract

Objective: make restrictions discoverable.

Docs should list:
- reserved fields
- why they exist
- suggested alternatives
- how raw document body is preserved
- examples that avoid reserved collisions.

Likely docs:
- `docs/data-models/documents.md`
- `docs/reference/sql-1-0-x.md`
- README quick examples.

### Task 4.2: Improve error message for reserved document fields

Objective: tell the user exactly what to do next.

Current:
`reserved system field 'kind' cannot be used as a top-level user field...`

Better:
`document field 'kind' is reserved for RedDB metadata. Rename it, e.g. 'event_type', or insert the raw JSON under body without top-level promotion. See docs/...`

Verification:
- unit/integration test asserts helpful error substring.

### Task 4.3: Evaluate raw-document preservation mode

Objective: decide if document insert can accept arbitrary JSON without promoting reserved keys.

Potential contract:
- `INSERT INTO events DOCUMENT VALUES ({"event_type":"signup"})` stores the document body (with schema-free keys); reserved field names are rejected at write time.
- Per [ADR 0066](../../.red/adr/0066-reserved-envelope-fields-user-pays.md), users rename colliding keys; the envelope stays unprefixed.

This is a product decision; defer code until contract is chosen.

---

## Phase 5: CLI first-run ergonomics

Problem: CLI is powerful, but early learning can be harder than necessary. `RETURNING *` sometimes prints only `insert ok`, making graph RIDs hard to discover.

### Task 5.1: Make `RETURNING *` visibly print returned rows in CLI

Objective: if a DML query has records, print them even when affected rows exist.

Test flow:
- run local query insert node returning `*`
- output contains `rid` and label/name fields.

Likely files:
- `src/bin/red.rs` query output formatting.
- CLI tests if present.

Verification:
- targeted CLI snapshot/integration test or direct command test.

### Task 5.2: Add `red demo` or `red playground` command

Objective: one command creates a small multi-model DB and prints copy-paste queries.

MVP behavior:
- `red demo --path ./demo.rdb`
- creates sample collections: users/products/events/social/jobs/metrics
- prints next queries to run
- no AI keys required

This can come after core correctness fixes.

---

## Phase 6: Docs and quickstart consolidation

Problem: docs mention `/health`, `/admin/health`, ports `55055`, `5000`, `55055`, etc. All may be valid, but first-run path feels fragmented.

### Task 6.1: Create one canonical quickstart

Objective: make a single 5-minute path the default.

Must include:
1. install/build/run local
2. start server
3. health/readiness check
4. `POST /query SELECT 1`
5. create table + insert + select
6. document insert
7. graph insert/traverse
8. queue push/pop
9. stop/restart and verify persistence

Likely files:
- `README.md`
- `docs/README.md`
- `docs/getting-started/quickstart.md` or existing getting-started page.

### Task 6.2: Add endpoint/port matrix

Objective: reduce confusion.

Table columns:
- Transport: HTTP, gRPC, RedWire, PG wire, admin/deep health
- Default port in binary
- Docker port
- Example command
- Auth behavior in dev/prod

---

## Phase 7: Hosted SaaS readiness

Objective: convert engine trust into cloud product trust.

Backlog:
- tenant model examples: shared table with `TENANT BY`, schema-based, dedicated DB file.
- backup/restore operator smoke.
- auth bootstrap UX for containers.
- metrics/doctor dashboard fields with stable severity.
- API-key lifecycle docs.
- golden path for “create hosted database, connect, run first query”.

---

## Recommended attack order

1. Phase 0: dogfood smoke harness.
2. Phase 1: fresh health/readiness.
3. Phase 2: graph label/RID happy path.
4. Phase 3: HTTP response shape decision/test.
5. Phase 4.1/4.2: document reserved-field docs + error message.
6. Phase 5.1: CLI `RETURNING *` output.
7. Phase 6: quickstart consolidation.
8. Phase 5.2 and Phase 7 after the first-run path is stable.

---

## Commands used during initial dogfood baseline

```bash
cargo run --bin red -- --help
/opt/cargo-target/debug/red query --help
/opt/cargo-target/debug/red server --help
cargo test --locked --test e2e_feedback_regression_pack -- --nocapture
cargo test --locked --test http_query_grimms_graph -- --nocapture
```

Manual server:
```bash
/opt/cargo-target/debug/red server \
  --dev \
  --path /tmp/reddb-server-dogfood/data.rdb \
  --http-bind 127.0.0.1:5000 \
  --grpc-bind 127.0.0.1:55055 \
  --no-log-file
```

Manual HTTP:
```bash
curl -i http://127.0.0.1:5000/health
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT 1"}'
```
