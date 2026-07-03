# reddb (Python)

Official Python driver for [RedDB](https://github.com/reddb-io/reddb).
Compiled Rust under the hood (via [pyo3](https://pyo3.rs)) — `import reddb`
gives you the engine in-process, no server, no subprocess, no extra deps.

## Install

```bash
pip install reddb
```

(After the first PyPI release. For now: see "Building from source" below.)

## Quickstart

```python
import reddb

db = reddb.connect("memory://")
# or:  reddb.connect("file:///var/lib/reddb/data.rdb")

db.insert("users", {"name": "Alice", "age": 30})
inserted = db.bulk_insert("users", [{"name": "Bob"}, {"name": "Carol"}])
print(inserted["rids"])

doc = db.documents.insert("profiles", {
    "name": "Hansel",
    "details": {"trail": "crumbs"},
})
print(db.documents.get("profiles", str(doc["rid"])))

db.kv.set("settings", "characters:hansel", {"role": "finder"})
print(db.kv.get("settings", "characters:hansel"))
print(db.kv.get("settings", "missing"))   # -> None (not an error)

db.queues.create("jobs")
db.queues.push("jobs", {"task": "reindex"})
print(db.queues.len("jobs"))              # -> 1
print(db.queues.pop("jobs")["items"])     # FIFO

# Transactions: imperative or callback. A clean callback commits; a raised
# exception rolls back and re-raises.
db.tx.run(lambda tx: db.insert("users", {"name": "Dave"}))

result = db.query("SELECT * FROM users")
for row in result["rows"]:
    print(row)

db.close()
```

You can also use it as a context manager:

```python
with reddb.connect("memory://") as db:
    db.insert("users", {"name": "Alice"})
    print(db.query("SELECT * FROM users"))
```

## Graph, vector and time-series

RedDB is one multi-model engine: graphs, vectors and time-series live in the
same database and are reached through `db.query()` (dedicated helpers arrive in
Helper Spec v1.1). Every snippet below is executed on each release by
[`tests/test_readme_examples.py`](tests/test_readme_examples.py).

**Graph** — nodes and edges in a `network` collection, then a shortest-path
traversal (the first user row gets `rid` 1024):

```python
db.query("INSERT INTO network NODE (label, node_type) VALUES ('gateway', 'Host')")
db.query("INSERT INTO network NODE (label, node_type) VALUES ('app', 'Host')")
db.query("INSERT INTO network NODE (label, node_type) VALUES ('db', 'Host')")
db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1025, 1.0)")
db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1025, 1026, 1.0)")
db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1026, 5.0)")

path = db.query("GRAPH SHORTEST_PATH '1024' TO '1026' ALGORITHM dijkstra")
path["rows"][0]["total_weight"]   # 2 — two 1.0 hops beat the single 5.0 edge
```

**Vector** — store embeddings, then rank by similarity. A Python list of floats
binds as a single `Vector` param:

```python
db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway runbook')")
db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual')")

hits = db.query("SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1", [1.0, 0.0])
hits["rows"][0]["score"]   # 1 — the identical vector scores exactly 1
```

**Time-series** — declare a series with retention + downsampling, ingest points
(timestamps are nanoseconds), then bucket them:

```python
db.query("CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg")
db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{\"host\":\"srv-a\"}', 0)")
db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{\"host\":\"srv-a\"}', 60000000000)")

rollup = db.query(
    "SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples "
    "FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)"
)
```

## Isolation levels

The default isolation is **snapshot isolation** (each transaction reads a
consistent MVCC snapshot). To request a stronger level, issue the isolation
clause with the opening statement via `db.query()`:

```python
db.query("BEGIN ISOLATION LEVEL SERIALIZABLE")
db.query("INSERT INTO audit (action) VALUES ('serializable write')")
db.query("COMMIT")
```

RedDB accepts the PG-compatible spellings `READ UNCOMMITTED`, `READ COMMITTED`,
`REPEATABLE READ` (= `SNAPSHOT`), and `SERIALIZABLE`. `SERIALIZABLE` engages the
serializable-snapshot-isolation (SSI) path, which can abort a transaction at
commit with a serialization conflict (see below).

## Serialization conflicts & retries

Under snapshot / serializable isolation, two transactions that write the same
row on overlapping snapshots resolve **first-committer-wins**: the later
transaction is aborted with a **retryable serialization conflict**. It surfaces
as a `ValueError` of the form `[QUERY_ERROR] serialization conflict: ...`. This
is the one error class you should catch and **retry** rather than surface to
the user:

```python
import time
import reddb

db = reddb.connect("file:///var/lib/reddb/data.rdb")

def is_serialization_conflict(err: Exception) -> bool:
    """True only for the retryable first-committer-wins conflict."""
    msg = str(err)
    return "QUERY_ERROR" in msg and "serialization conflict" in msg.lower()

def with_retry(fn, max_retries: int = 5):
    """Run a transaction, retrying with backoff on serialization conflicts."""
    attempt = 0
    while True:
        try:
            return db.tx.run(fn)
        except ValueError as err:
            if is_serialization_conflict(err) and attempt < max_retries:
                attempt += 1
                time.sleep(2 ** attempt * 0.005)  # backoff
                continue
            raise  # not retryable, or out of attempts

# Concurrent debits stay consistent — losers retry against the fresh snapshot.
def debit(tx):
    balance = db.query("SELECT balance FROM accounts WHERE id = $1", 1)["rows"][0]["balance"]
    db.query("UPDATE accounts SET balance = $1 WHERE id = $2", balance - 10, 1)

with_retry(debit)
```

Only conflicts are retried — a syntax error, a constraint violation, or any
other `QUERY_ERROR` propagates on the first attempt.

## Parameterized queries

`db.query(sql, *params)` binds positional `$N` placeholders without
splicing values into the SQL string — the engine evaluates a parsed
expression with typed values, so quoting bugs and SQL injection are
not on the menu.

```python
import datetime, uuid
import reddb

with reddb.connect("memory://") as db:
    db.query("CREATE TABLE users (id INT, name TEXT, age INT)")
    db.insert("users", {"id": 1, "name": "Alice", "age": 30})
    db.insert("users", {"id": 2, "name": "Bob",   "age": 25})

    # variadic positional form
    rows = db.query("SELECT * FROM users WHERE id = $1", 2)["rows"]

    # keyword form — lists and tuples both produce the same wire call
    rows = db.query("SELECT * FROM users WHERE id = $1", params=[2])["rows"]
    rows = db.query("SELECT * FROM users WHERE id = $1", params=(2,))["rows"]

    # vector parameter for SEARCH SIMILAR (a list of floats binds as one Vector)
    db.query(
        "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2",
        [0.1, 0.2, 0.3], 10,
    )

    # bytes / timestamp / uuid are first-class
    db.query("SELECT * FROM blobs WHERE blob = $1", b"\\x00\\x01\\x02")
    db.query("SELECT * FROM events WHERE at = $1", datetime.datetime(2026, 5, 12))
    db.query("SELECT * FROM users WHERE uid = $1",
             uuid.UUID("12345678-1234-5678-1234-567812345678"))
```

### Native type mapping

| Python                                | Engine `Value`     |
|---------------------------------------|--------------------|
| `None`                                | `Null`             |
| `bool`                                | `Boolean`          |
| `int`  (`-2^63 .. 2^63-1`)            | `Integer`          |
| `int`  (`2^63 .. 2^64-1`)             | `UnsignedInteger`  |
| `float`                               | `Float`            |
| `str`                                 | `Text`             |
| `bytes`, `bytearray`                  | `Blob`             |
| `list[float|int]`                     | `Vector` (f32)     |
| `datetime.datetime`                   | `Timestamp` (sec)  |
| `uuid.UUID`                           | `Uuid` (16 bytes)  |
| `dict[str, scalar]`                   | `Json`             |

Anything else raises `ValueError("[INVALID_PARAMS] ...")` so a typo
(`set`, unsupported numpy scalar) fails loud instead of
silently coercing.

> Parameterized queries currently require the embedded backend
> (`memory://` or `file://`). Over `grpc://` they raise
> `[PARAMS_UNSUPPORTED] ...` until the gRPC server advertises
> `FEATURE_PARAMS` (tracked alongside the Rust client work in #364).

## Connection URIs

| URI                       | Backend                          | Status        |
|---------------------------|----------------------------------|---------------|
| `memory://`               | Ephemeral in-memory engine       | ✅            |
| `file:///absolute/path`   | Embedded engine on disk          | ✅            |
| `grpc://host:port`        | Remote gRPC server               | ✅            |

The high-level `reddb.connect("grpc://host:port")` API speaks the
official tonic-backed gRPC client and is the recommended path for
remote connections. The low-level `legacy_grpc_connect` API below is
kept around for power users who need direct access to the
generated-protobuf `Connection` class.

## API

### High-level (recommended)

```python
import reddb

db = reddb.connect(uri: str) -> RedDb

db.query(sql: str, *params)                      -> {"statement", "affected", "columns", "rows"}
db.query(sql: str, params=[...] | (...))         -> {"statement", "affected", "columns", "rows"}
db.execute(sql: str, *params)                    -> {"statement", "affected", "columns", "rows"}
db.execute(sql: str, params=[...] | (...))       -> {"statement", "affected", "columns", "rows"}
db.insert(collection: str, payload: dict)        -> {"affected", "rid", "id"}
db.bulk_insert(collection: str, payloads: list[dict]) -> {"affected", "rids", "ids"}
db.get(collection: str, rid: str)                -> item | None
db.exists(collection: str, rid: str)             -> {"exists"}
db.list(collection: str, *, limit, filter, order_by) -> {"items"}
db.delete(collection: str, rid: str)             -> {"affected"}
db.documents.insert(collection: str, document: dict) -> {"affected", "rid", "item"}
db.documents.get(collection: str, rid: str)      -> item
db.documents.list(collection: str, *, limit, filter, order_by) -> {"items"}
db.documents.patch(collection: str, rid: str, patch: dict) -> item
db.documents.delete(collection: str, rid: str)   -> {"affected", "deleted"}
db.kv.set(collection: str, key: str, value)      -> {"affected"}
db.kv.get(collection: str, key: str)             -> value | None
db.kv.exists(collection: str, key: str)          -> {"exists"}
db.kv.delete(collection: str, key: str)          -> {"affected", "deleted"}
db.kv.list(collection: str, *, prefix, limit)    -> {"items"}
db.queues.create(name: str)                      -> {"affected"}
db.queues.push(name: str, payload)               -> {"affected"}
db.queues.peek(name: str, limit=None)            -> {"items", "affected"}
db.queues.pop(name: str, limit=None)             -> {"items", "affected"}
db.queues.len(name: str)                         -> int
db.queues.purge(name: str)                       -> {"affected", "deleted"}
db.tx.begin()                                    -> {"affected"}
db.tx.commit()                                   -> {"affected"}
db.tx.rollback()                                 -> {"affected"}
db.tx.run(callback)                              -> callback return value
db.helper_spec_version                           -> "1.0"
db.health()                                      -> {"ok": True, "version": str}
db.version()                                     -> {"version": str, "protocol": "1.0"}
db.close()                                       -> None
```

`db.queue` is a singular alias for `db.queues` (same client). `reddb.helper_spec_version`
is also exposed at module level for CI assertions.

Row `payload` values must be `None`, `bool`, `int`, `float` or `str`.
Document and KV helpers accept JSON-compatible nested dicts/lists. Public
identity is always `rid`; `id` and `ids` are compatibility aliases on row
insert results only.

Helper availability:

| Helper group | `memory://` / `file://` | `grpc://` |
|--------------|--------------------------|-----------|
| query/execute without params | supported | supported |
| query/execute with params | supported | raises `PARAMS_UNSUPPORTED` |
| row insert/bulk/get/list/delete | supported | supported except parameterized internals |
| documents | supported | raises `NOT_SUPPORTED`; use `query()` until remote helper parity lands |
| kv | supported | raises `NOT_SUPPORTED`; use `query()` until remote helper parity lands |
| queues | supported | raises `NOT_SUPPORTED`; use `query()` until remote helper parity lands |
| tx | supported | raises `NOT_SUPPORTED`; use `query()` until remote helper parity lands |

Conformance command:

```bash
python -m pytest drivers/python/tests/test_smoke.py drivers/python/tests/test_helpers.py \
  drivers/python/tests/test_documents_conformance.py \
  drivers/python/tests/test_conformance.py \
  drivers/python/tests/test_readme_examples.py
```

### SDK Helper Spec conformance

This driver is conformant with [`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md)
v1.0 on the embedded transports (`memory://`, `file://`). The full §12 case
list is ported in [`tests/test_conformance.py`](tests/test_conformance.py).
`db.helper_spec_version` (and the module attribute `reddb.helper_spec_version`)
returns `"1.0"`.

**Return envelopes.** `insert` → `{affected, rid, id}`; `bulk_insert` →
`{affected, rids, ids}`; `documents.delete` / `kv.delete` / `queues.purge` →
`{affected, deleted}`; `exists` → `{exists}`; `list` / `queues.peek` /
`queues.pop` → `{items, ...}`; `kv.get` returns the value or `None`. Wire field
names (`rid`, `affected`, `deleted`, `items`) are preserved.

**Transaction support: imperative + callback.** `db.tx.begin/commit/rollback`
are session-stateful on the connection. `db.tx.run(callback)` commits on a
clean return and rolls back + re-raises on exception. Nested `tx.run` is
rejected with `INVALID_ARGUMENT` — issue savepoints via `db.query` directly
if you need them (spec §7.2).

| Case ID                              | Status     |
|--------------------------------------|------------|
| `generic.query.no_params`            | supported  |
| `generic.query_with.params`          | supported  |
| `generic.insert.rid`                 | supported  |
| `generic.bulk_insert.rids`           | supported  |
| `generic.delete`                     | supported  |
| `documents.crud_nested_patch`        | supported  |
| `documents.delete_missing_no_error`  | supported  |
| `documents.patch_empty_rejects`      | supported  |
| `kv.exact_key_round_trip`            | supported  |
| `kv.missing_get_returns_none`        | supported  |
| `kv.delete_returns_envelope`         | supported  |
| `queues.fifo_peek_pop_len`           | supported  |
| `queues.empty_pop_returns_empty`     | supported  |
| `queues.purge_resets_len`            | supported  |
| `tx.commit_persists`                 | supported  |
| `tx.rollback_discards`               | supported  |
| `errors.invalid_argument.empty_sql`  | supported  |
| `errors.not_found.document_get`      | supported  |
| `wire.vectors.sql_round_trip`        | provisional (raw `query()`) |
| `wire.graph.sql_round_trip`          | provisional (raw `query()`) |
| `wire.timeseries.sql_round_trip`     | provisional (raw `query()`) |
| `wire.probabilistic.hll_round_trip`  | provisional (raw `query()`) |

**Out of scope in v1.0 helpers** (reachable via raw `db.query` until lifted
into helpers in v1.x, per spec §8–§11): first-class `vectors.*`, `graph.*`,
`timeseries.*`, and `probabilistic.*` helpers (see [Graph, vector and
time-series](#graph-vector-and-time-series)); queue priority / consumer
groups / dead-letter routing (§6); `kv.expire` TTL helper and gRPC
`kv.watch` (§5); cross-shard transactions (§7). Transaction **isolation
levels** and the retryable serialization-conflict class are available today
through raw `db.query` — see [Isolation levels](#isolation-levels) and
[Serialization conflicts & retries](#serialization-conflicts--retries).
Helpers are embedded-only today; over `grpc://` they raise `NOT_SUPPORTED`.

### Low-level (advanced)

The original gRPC and raw-wire clients are still exported for power users:

```python
import reddb

# gRPC against a remote server (returns reddb.Connection)
conn = reddb.legacy_grpc_connect("127.0.0.1:55055")
conn.query("SELECT * FROM users")
conn.close()

# Raw TCP wire protocol (fastest, returns reddb.WireConnection)
wc = reddb.wire_connect("127.0.0.1:5050")
```

These mirror the `Connection` and `WireConnection` classes that shipped before
the unified API.

## Errors

The high-level API raises `ValueError` with a message of the form
`[CODE] message`. Stable codes (mirroring the JSON-RPC stdio protocol):

| code                  | meaning                                                    |
|-----------------------|------------------------------------------------------------|
| `INVALID_URI`         | URI malformed                                              |
| `UNSUPPORTED_SCHEME`  | Scheme not recognized                                      |
| `INVALID_PARAMS`      | A method argument has the wrong type                       |
| `INVALID_ARGUMENT`    | Helper input is malformed (empty SQL, empty patch, …)      |
| `NOT_FOUND`           | A required item, rid, or document is absent                |
| `NOT_SUPPORTED`       | Helper not available on this transport (e.g. `grpc://`)    |
| `QUERY_ERROR`         | SQL parse or execution failure                             |
| `IO_ERROR`            | Disk / file backend failure                                |
| `FEATURE_DISABLED`    | Caller hit a path gated behind a Cargo feature             |
| `CLIENT_CLOSED`       | Method called after `close()`                              |

## Building from source

You need a Rust toolchain and [maturin](https://www.maturin.rs).

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin
cd drivers/python
maturin develop --release
python -c "import reddb; print(reddb.connect('memory://').version())"
```

## Limits

- The wheel statically links the entire RedDB engine. Expect ~10 MB+ per arch.
- No async API yet — every call is blocking (releases the GIL during query
  execution so other Python threads can run).
- No connection pooling. The handle is `Send + Sync` on the Rust side; in
  Python you can share a `RedDb` between threads.
- `query` returns the full result set as a Python list. Streaming is on the
  roadmap.

## Production deploy

When you're ready to point this driver at a production RedDB cluster:

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use `reds://` (TLS)** or `red://...?tls=true` for any traffic
  crossing the network — never plain `red://` outside localhost.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> Driver-helper (SDK Helper Spec v1.0) support for every public promise. A helper not marked supported here is not promised by this driver.

| Promise | driver_helpers |
| --- | --- |
| **PSC-001** — RedDB is one multi-model database (tables, graph, KV, timeseries, probabilistic, vector, queue, documents) backed by a single file. | ✅ supported |
| **PSC-002** — MATCH supports node, edge, label, property, and LIMIT projections. | ✅ supported |
| **PSC-003** — GRAPH algorithms accept semantic identifiers, limits, ordering, and return stable rich rows. | ❌ unsupported |
| **PSC-004** — INSERT creates rows, documents, and native timeseries points. | ✅ supported |
| **PSC-005** — HLL/SKETCH/FILTER expose write and read commands for cardinality, frequency, and membership. | ⚠️ partial |
| **PSC-006** — Timeseries stores timestamped metrics with tags and supports query/readback. | ⚠️ partial |
| **PSC-007** — Documents are first-class: create, read, update, delete, and SQL analytics over JSON. | ✅ supported |
| **PSC-008** — KV helpers expose get/put/delete; get of a missing key returns null, delete reports affected. | ✅ supported |
| **PSC-009** — Queue helpers expose create/push/peek/pop/len/purge with FIFO semantics; empty pop is not an error. | ✅ supported |
| **PSC-010** — Transactions are imperative (begin/commit/rollback) plus a run(callback) form; empty SQL rejects with INVALID_ARGUMENT. | ✅ supported |
| **PSC-011** — SQL aggregate, projection, expression, and mutation behaviour matches ordinary SQL expectations where advertised. | ✅ supported |
| **PSC-012** — Server transports expose the same query contract as embedded (HTTP, RedWire, gRPC parity). | ✅ supported |
| **PSC-013** — Official drivers implement the SDK Helper Spec v1.0 conformance suite (all 22 §12 case IDs). | ✅ supported |
| **PSC-014** — ASK / SEARCH semantic surfaces return ranked results with stable shape. | ⚠️ partial |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
