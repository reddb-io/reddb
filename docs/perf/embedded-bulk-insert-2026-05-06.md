# Embedded `bulk_insert` profile — 2026-05-06

Status: **static analysis pending live profile**.

Tracking issue: #93 — *"embedded `bulk_insert` path executes one SQL
statement per row instead of batching"*.

> **Reproducing the live profile.** Same host kernel knobs that blocked
> the #76 slice still apply
> (`kernel.perf_event_paranoid = 4`, `kernel.yama.ptrace_scope = 1`).
> Loosen them per [`perf-knobs.md`](perf-knobs.md), then drive the
> embedded path through `rdb-benchmark`'s `reddb_embedded` adapter
> (`crates/bench-adapters/src/local.rs:130`).

## TL;DR

- The embedded driver's `bulk_insert` is a **lie**: it serialises each
  payload to an `INSERT INTO …` SQL string and calls
  `runtime.execute_query` once per row
  (`crates/reddb-client/src/embedded.rs:76-93`). Every row therefore
  pays full SQL **lex + parse + plan + autocommit + WAL fsync wait**,
  with **zero amortisation** between rows.
- The other two driver shapes already batch: `grpc::bulk_insert`
  (`crates/reddb-client/src/grpc.rs:158-177`) routes through the
  server's `BulkCreateRows` RPC →
  `runtime.create_rows_batch(CreateRowsBatchInput { rows })` (one
  call); `redwire::bulk_insert_binary`
  (`crates/reddb-client/src/redwire/mod.rs:381-441`) sends a single
  binary frame and the server's `handle_bulk_insert_binary`
  (`crates/reddb-server/src/wire/listener.rs:432`) calls
  `runtime.create_rows_batch_columnar(collection, column_names, rows)`
  exactly once.
- Embedded ought to skip the wire entirely and call
  `runtime.create_rows_batch_columnar` (or
  `create_rows_batch_prevalidated_columnar` when no contract is
  declared) directly. Same kernel the RedWire fast path already lands
  on.
- Live CPU profile could not be produced from this slice for the same
  reason as #76: `perf_event_paranoid = 4`, `ptrace_scope = 1`,
  bench `red` runs in Docker. Re-run with `make perf-bench` once the
  knobs are loosened. The ranking below is a static read.

## Headline (static, ops/sec)

We have no live embedded number on this slice — the `reddb_embedded`
adapter is gated behind the `bench-adapters/reddb-local` Cargo
feature and a fresh build. Numbers below are extrapolations from
the #76 wire/gRPC measurements (`docs/perf/insert_sequential-2026-05-05.md`),
keyed on the per-row cost difference between *one batched call* and
*N autocommit `execute_query` calls*. Mark each `?` for the live
profile pass.

| path                                  | 1k items   | 10k items | notes                                   |
|---------------------------------------|-----------:|----------:|-----------------------------------------|
| RedWire `bulk_insert_binary` (#76)    | ≈ 6 367    | ≈ 1 367   | one frame, columnar kernel              |
| gRPC `bulk_insert` (BulkCreateRows)   | ?          | ?         | one RPC, JSON parse server-side         |
| **Embedded `bulk_insert` (this bug)** | **≪ 6 367**| **≪ 1 367** | one SQL exec **per row**, in-process |

Why "≪": the embedded path skips network framing entirely (which is
the only thing it would gain from being in-process), but it pays an
extra cost the wire path does not: **N × full SQL parser + planner +
autocommit cycle**. The parser walk alone is dozens of allocations
per row for a typical 7-column `bench_users` insert. Compared to the
columnar fast path that the wire frame already routes through, this
is several multiples of overhead. `insert_sequential` (`insert_one`
in a loop) already costs ~109–840 µs p50 on the wire path; the
embedded `bulk_insert` adds the parser/planner cost on top of that
same per-row WAL fsync.

## Per-row SQL execution path (file:line)

The bug is local to one function; the path it walks is the entire
SQL stack.

### The bug — `crates/reddb-client/src/embedded.rs:76-93`

```rust
pub fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
    let mut total = 0u64;
    for payload in payloads {                                       // ← per-row loop
        let object = payload.as_object().ok_or_else(/* … */)?;
        let sql = build_insert_sql(collection, object);             // ← :85, build SQL string
        let qr = self
            .runtime
            .execute_query(&sql)                                    // ← :88, one SQL stmt per row
            .map_err(/* … */)?;
        total += qr.affected_rows;
    }
    Ok(total)
}
```

The accompanying `build_insert_sql` at `embedded.rs:115-127` does
`format!("INSERT INTO {collection} ({}) VALUES ({})", …)` with a
fresh `Vec<String>` of column names + value literals per row.
`value_to_sql_literal` at `:129-145` then *re-stringifies* each value
(including JSON for arrays/objects) so the parser can immediately
re-parse it.

### What `runtime.execute_query` pays per call

Anchors: `crates/reddb-server/src/runtime/impl_core.rs:3563`.
Per autocommit `INSERT … VALUES (…)` invocation:

1. `try_fast_entity_lookup` short-circuit check — cheap, but rejects
   inserts (it's the SELECT-by-id fast path,
   `impl_core.rs:3574-3586`).
2. `try_strip_within_prefix` — string scan,
   `impl_core.rs:3594-3601`.
3. SQL **lex + parse** into AST. INSERTs flow through the full
   statement parser; 7-column row produces ~30–50 AST node
   allocations.
4. **Plan** — `runtime/impl_dml.rs` INSERT path normalises the
   column list against the catalog, builds a `CreateRowInput`,
   resolves any contract.
5. `MutationEngine::apply` (`runtime/mutation.rs:66`) sees
   `rows.len() == 1` and dispatches to `append_one`
   (`runtime/mutation.rs:89`) — i.e. the **same single-row kernel
   `insert_sequential` already profiled in #76**.
6. Snapshot xid `begin()` + `commit()` (`runtime/mutation.rs:111-118`).
7. `store.insert_auto`
   (`storage/unified/store/impl_entities.rs:699`) — `manager.insert`
   then `manager.get(id)` clone + `btree.insert` + WAL append.
8. **WAL group-commit fsync wait**
   (`storage/unified/store/commit.rs:399-417`,
   `wait_until_durable` at `:513`). With
   `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0`
   (`crates/reddb-server/src/api.rs:36`) and one in-process caller,
   every row blocks on its own fsync.

So a 1 000-row embedded `bulk_insert` is 1 000 SQL parses + 1 000
planner walks + 1 000 xid begin/commit pairs + 1 000 fsyncs. The
batched RedWire path is 1 SQL-shape decode + 1 batch entry into
`create_rows_batch_columnar` + 1 xid + 1 fsync (group-commit) for the
whole batch.

## Root cause

`crates/reddb-client/src/embedded.rs` only depends on
`reddb_server::runtime::RedDBRuntime` for one operation — `execute_query`.
It never imports `application::CreateRowsBatchInput` or any of the
typed batch ports, so the only tool it has is "hand the runtime a
SQL string". The author chose the smallest-diff implementation: build
a SQL string per row and feed it through `execute_query`. That is
correct for `insert(single)` but pathological for `bulk_insert`.

The right batch entry points already exist and are public on the
runtime via the application port impl
(`crates/reddb-server/src/application/ports_impls_entity.rs`):

- `create_rows_batch(CreateRowsBatchInput)` — `:2082`. The general
  batch path; takes `Vec<CreateRowInput>` (string-keyed fields).
- `create_rows_batch_columnar(collection, column_names, rows)`
  — `:2256`. Decoupled column names + `Vec<Vec<Value>>`. Fast-paths
  through `create_rows_batch_prevalidated_columnar` when the
  collection has no declared contract (the bench-adapter case).
  This is the exact entry the wire frame at
  `wire/listener.rs:432` uses, landed by #76 commit `1ce978c`
  ("feat(runtime): add `create_rows_batch_columnar` entity port (#76)")
  and `27e23e1` ("perf(wire): skip per-row String allocs in
  `handle_bulk_insert_binary`").
- `create_rows_batch_prevalidated_columnar` — `:2147`. Same as above
  but assumes the caller already validated types. Available if the
  embedded driver wants the maximum-throughput shape.

The fix is "stop building SQL; build a `CreateRowsBatchInput` once
and call the batch port". No new server-side code.

## Top suspected bottlenecks (ranked by expected weight)

Same caveat as `insert_sequential-2026-05-05.md`: without a
flamegraph the ranking is best-effort. Each item carries its own
validation step.

1. **Per-row full-SQL parse + plan inside `execute_query`.** This is
   the dominant cost the embedded path adds *on top of* what
   `insert_sequential` already pays. A 7-column `INSERT … VALUES (…)`
   round-trips through the lexer and AST builder, allocates dozens
   of `String`s and AST nodes, then immediately throws them away —
   N times for an N-row batch. Validation: `cargo bench -p
   reddb-server execute_query` on a fixed INSERT shape gives a
   per-call floor; multiply by N to compare against the columnar
   path.
2. **Per-row WAL group-commit fsync wait.** Each `execute_query` is
   its own autocommit, so each row pays one fsync (same as #76 P1).
   The columnar batch path issues one WAL append for the whole
   batch and pays exactly one fsync. With consumer NVMe at ~50–
   150 µs per fsync, 1 000 rows ≈ 50–150 ms of pure fsync wait the
   batch path eliminates. This is the *largest* absolute time
   saving.
3. **Per-row `build_insert_sql` allocations + per-row
   `value_to_sql_literal` re-stringification.** Every row formats
   the JSON value back into a SQL literal (`embedded.rs:115-145`),
   which the runtime parser then re-parses into a `SchemaValue`.
   For string-heavy rows this is a full extra `String::clone` +
   escape pass per cell. Validation: alloc-instrumented test with a
   10×7 input set — expectation is zero `String::from(JsonValue)`
   per row once the fix routes through `create_rows_batch_columnar`.

Honourable mentions (already covered by #76 punch-list — they apply
to the embedded path too once items 1–3 are addressed):
- Snapshot manager `begin()` + `commit()` per autocommit row
  (#76 P4).
- `manager.get(id)` clone after `manager.insert` in `insert_auto`
  (#76 P2).

## Punch-list

Concrete follow-ups bounded so a single PR can land each. **Do not
file as issues from this slice — main agent reviews and decides.**

### B1 — Route embedded `bulk_insert` through `create_rows_batch_columnar`

- **Problem.** `embedded.rs:76-93` loops `execute_query` per row.
- **Suspected fix.** Replace the loop with: (a) collect the
  union of column names across `payloads` into a single
  `Arc<Vec<String>>`; (b) build `Vec<Vec<SchemaValue>>` by mapping
  each `JsonValue` to its `SchemaValue` (already partially done by
  the SQL-literal helper — invert the mapping); (c) call
  `runtime.create_rows_batch_columnar(collection.into(), column_names, rows)`
  once. Mirror the path used by `wire/listener.rs:432` (commit
  `27e23e1`). For collections with no declared contract this
  fast-paths to `create_rows_batch_prevalidated_columnar` for free
  (`ports_impls_entity.rs:2293`).
- **Acceptance.** A 1 000-row embedded `bulk_insert` issues exactly
  one WAL append (`storage/unified/store/commit.rs:399`); ops/sec
  on the bench `reddb_embedded` adapter matches RedWire within
  10–20% (it should *beat* RedWire on small batches because no
  framing/handshake — but never lose).
- **Risk.** JSON → `SchemaValue` mapping must accept the same shapes
  `parse_create_row_input` does
  (`crates/reddb-server/src/application/entity_payload.rs`); the
  cleanest move is to call that helper once per row to get the
  `(String, Value)` tuple list, then re-arrange into the columnar
  shape — still O(N) but no SQL parsing.

### B2 — Replace embedded `insert(single)` autocommit with the same batch port

- **Problem.** `embedded.rs:58-74` is the same antipattern as B1
  but for one row: build SQL, hand to `execute_query`. It's the
  base-case for B1.
- **Suspected fix.** Once B1 lands, route `insert` through a 1-row
  `CreateRowsBatchInput`. Avoids the parser entirely on the
  hot autocommit insert.
- **Acceptance.** `embedded::insert` no longer touches
  `runtime::execute_query`; in-process p50 for `insert_one`
  matches `MutationEngine::append_one` overhead within fsync
  variance.

### B3 — Embedded driver doc: pin "use `bulk_insert` for hot writes"

- **Problem.** Once B1 lands, `bulk_insert` is N× faster than a
  loop of `insert`. Without doc guidance, callers will keep
  hand-rolling the loop.
- **Suspected fix.** README + `EmbeddedClient::bulk_insert` rustdoc
  example: "for ≥10 rows, prefer `bulk_insert`; one WAL fsync, one
  xid".
- **Acceptance.** Single rustdoc edit; no test. Punch-list item is
  bounded enough that the main agent can fold it into B1.

### B4 — Embedded `bulk_insert` regression test asserting one WAL append

- **Problem.** Nothing pins the batching invariant; a future
  refactor could regress to per-row execution silently.
- **Suspected fix.** New test in
  `crates/reddb-client/tests/embedded_bulk_insert.rs`: open
  `EmbeddedClient::in_memory`, do `bulk_insert` of 100 rows, assert
  via a runtime metric (`runtime.metrics().wal_appends_total` or
  equivalent — wire it up if missing) that the call produced
  exactly one WAL append.
- **Acceptance.** Test is red on current main, green after B1.
- **Risk.** If no per-call WAL counter exists yet, the test has to
  use a CDC-tap or a snapshot of `commit_log` LSN before/after.

### B5 — Live profile of embedded `bulk_insert` (post-fix sanity check)

- **Problem.** This document is static analysis. Until a flamegraph
  is captured, the ranking above is best-guess.
- **Tooling.** Reuse `make perf-bench` from #76 P5
  (`Makefile`, target writes `target/perf/insert_sequential.svg`).
  Wire a sibling target `make perf-bench-embedded` that drives the
  `reddb_embedded` adapter directly — no Docker involved, so the
  `perf_event_paranoid` blocker becomes a host-level decision
  rather than a Docker-isolation one.
- **Acceptance.** Flamegraph shows zero `Parser::parse_statement`
  in the embedded `bulk_insert` hot path post-B1; SQL parse cost
  drops to <1% of CPU.

## Notes for the next investigator

- The fix is **one file** (`crates/reddb-client/src/embedded.rs`);
  no server-side changes — the runtime port already exists.
- `build_insert_sql` and `value_to_sql_literal` become dead code
  once B1 + B2 land; remove in the same PR.
- The single-row `embedded::insert` (`:58-74`) hits the same
  pathology in miniature and is worth folding into the same fix.
- Bench coverage for the embedded path is already wired:
  `rdb-benchmark/crates/bench-adapters/src/local.rs:130` calls
  `EmbeddedClient::bulk_insert` directly. Re-running
  `insert_sequential` against `reddb_embedded` after the fix gives
  the headline number this doc had to leave as `?`.
