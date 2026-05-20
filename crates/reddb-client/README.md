# reddb-io-client

Official Rust client for [RedDB](https://github.com/reddb-io/reddb).
One connection-string API across embedded, gRPC, HTTP, and
RedWire transports. Also hosts the `red_client` binary and the
workspace-internal connector used by `red`'s REPL and
`reddb-server`'s rpc_stdio mode.

## Quickstart (library)

```toml
[dependencies]
reddb-io-client = "1.2"
```

```rust,no_run
use reddb_client::{Reddb, JsonValue};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;
db.insert("users", &JsonValue::object([("name", JsonValue::string("Alice"))])).await?;
let result = db.query("SELECT * FROM users").await?;
println!("{} rows", result.rows.len());
db.close().await?;
# Ok(())
# }
```

### Rich helpers

The Rust client is the **reference implementation** of the
[SDK Helper Spec v1.0](../../docs/spec/sdk-helpers.md). The exact spec version
this driver tracks is exported as a constant (spec §14):

```rust
assert_eq!(reddb_client::HELPER_SPEC_VERSION, "1.0");
```

`documents.*`, `kv.*`, `queues.*`, and `tx.*` are first-class; patch is a
top-level merge, an empty patch returns `INVALID_ARGUMENT`, an empty `query`
returns `INVALID_ARGUMENT`, and `documents.delete` / `kv.delete` of a missing
item return `{ affected: 0, deleted: false }` without raising.

**Transaction support:** imperative only — `db.begin()`, `db.commit()`,
`db.rollback()` (spec §7.1). There is no callback `tx.run` form (§7.2);
callers wanting savepoints issue them directly via `db.query`.

#### Return envelopes (spec §2.4)

| Envelope            | Fields surfaced                                  |
|---------------------|--------------------------------------------------|
| `QueryResult`       | `columns`, `rows`, `affected`                    |
| `InsertResult`      | `affected` (1), `rid`                            |
| `BulkInsertResult`  | `affected`, `rids` (input order)                 |
| `DeleteResult`      | `affected`, `deleted` (`affected > 0`)           |
| `ExistsResult`      | `exists`                                         |
| `ListResult`        | `items`, `affected`                              |
| `DocumentItem`      | `rid`, `fields`                                  |
| `KvItem`            | `collection`, `key`, `value`                     |

#### Conformance matrix (spec §12)

Every case ID below is wired in `tests/conformance.rs` and runs against
**both** transports the helper surface targets — embedded (`memory://`,
always) and a live client (`red://` gRPC, gated):

| Case ID                              | Status |
|--------------------------------------|--------|
| `generic.query.no_params`            | ✅ |
| `generic.query_with.params`          | ✅ |
| `generic.insert.rid`                 | ✅ |
| `generic.bulk_insert.rids`           | ✅ |
| `generic.delete`                     | ✅ |
| `documents.crud_nested_patch`        | ✅ |
| `documents.delete_missing_no_error`  | ✅ |
| `documents.patch_empty_rejects`      | ✅ |
| `kv.exact_key_round_trip`            | ✅ |
| `kv.missing_get_returns_none`        | ✅ |
| `kv.delete_returns_envelope`         | ✅ |
| `queues.fifo_peek_pop_len`           | ✅ |
| `queues.empty_pop_returns_empty`     | ✅ |
| `queues.purge_resets_len`            | ✅ |
| `tx.commit_persists`                 | ✅ |
| `tx.rollback_discards`               | ✅ |
| `errors.invalid_argument.empty_sql`  | ✅ |
| `errors.not_found.document_get`      | ✅ |
| `wire.vectors.sql_round_trip`        | ✅ (provisional, SQL-only) |
| `wire.graph.sql_round_trip`          | ✅ (provisional, SQL-only) |
| `wire.timeseries.sql_round_trip`     | ✅ (provisional, SQL-only) |
| `wire.probabilistic.hll_round_trip`  | ✅ (provisional, SQL-only) |

#### Out-of-scope in v1.0 (reach via raw `query` / `query_with`)

- **Vectors / Graph / Time-series / Probabilistic** — no first-class helpers;
  the wire SQL surface is stable (spec §§8–11). First-class helpers land in
  v1.1.
- **`kv.expire` / TTL helper** — reach via `WITH TTL` on the underlying put.
- **Queue priority / consumer groups / dead-letter** — reach via raw
  `CREATE QUEUE … PRIORITY` / `QUEUE POP … GROUP`.
- **Callback `tx.run`, isolation-level `begin`, cross-shard tx** — deferred.

#### Running the conformance harness

```sh
# Embedded transport (memory://) — always runs:
cargo test -p reddb-io-client --test conformance

# Add the live client transport (red:// over gRPC):
RED_SMOKE=1 RED_BIN=/path/to/red \
  cargo test -p reddb-io-client --features grpc --test conformance

# Embedded helper tour (runnable example):
cargo run -p reddb-io-client --example embedded_helpers
```

```rust,no_run
use reddb_client::{JsonValue, ListOptions, Reddb};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;

let doc = db
    .documents()
    .insert(
        "events",
        &JsonValue::object([
            ("event_type", JsonValue::string("login")),
            ("attempts", JsonValue::number(1.0)),
        ]),
    )
    .await?;
let _same_doc = db.documents().get("events", &doc.rid).await?;
let _recent = db
    .documents()
    .list("events", ListOptions::new().filter("event_type = 'login'").limit(10))
    .await?;

let kv = db.kv_collection("settings");
kv.set("characters:hansel", JsonValue::string("trail")).await?;
let _value = kv.get("characters:hansel").await?;

let queue = db.queue();
queue.create("jobs").await?;
queue.push("jobs", &JsonValue::object([("kind", JsonValue::string("email"))])).await?;
let _jobs = queue.pop("jobs").await?;
# Ok(())
# }
```

### Parameterized queries

`query_with(sql, &[params])` binds positional `$N` placeholders. SQL
literals never carry untrusted values — vector / int / text params
travel through the same engine binder the prepared-statement path uses,
so injection-by-string is structurally impossible.

```rust,no_run
use reddb_client::{Reddb, Value};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;

// Scalars use IntoValue conversions:
let rows = db
    .query_with(
        "SELECT * FROM users WHERE id = $1 AND name = $2",
        &[1i64.into(), "Alice".into()],
    )
    .await?;

// Vector params route through `Value::Vector` (no string-formatting):
let hits = db
    .query_with(
        "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2",
        &[Value::Vector(vec![0.1, 0.2, 0.3]), Value::Int64(5)],
    )
    .await?;
# let _ = (rows, hits);
# Ok(())
# }
```

The native RedWire client uses the same parameter API when the crate is built
with `--features redwire`:

```rust,no_run
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::Value;

# async fn run() -> reddb_client::Result<()> {
let mut client = RedWireClient::connect(
    ConnectOptions::new("127.0.0.1", 5050).with_auth(Auth::Anonymous),
)
.await?;

let rows = client
    .query_with("SELECT $1", &[Value::Int64(42)])
    .await?;
# let _ = rows;
# Ok(())
# }
```

Native Rust → engine `Value` mapping:

| Rust                  | Engine variant          |
|-----------------------|-------------------------|
| `bool`                | `Boolean`               |
| `i8..i64` / `u8..u32` | `Integer` (i64)         |
| `f32` / `f64`         | `Float` (f64)           |
| `&str` / `String`     | `Text`                  |
| `Vec<u8>` / `&[u8]`   | `Blob`                  |
| `Vec<f32>` / `&[f32]` | `Vector`                |
| `Option<T>` (None)    | `Null`                  |
| `serde_json::Value`   | `Json`                  |
| `Value::Int64(n)` | `Int` (i64 compatibility constructor) |
| `Value::Timestamp(s)` | `Timestamp` (seconds)   |
| `Value::Uuid(b)`      | `Uuid` (16 raw bytes)   |

Today the `embedded`, `http`, and Rust `grpc` transports carry parameters
end-to-end. The native Rust RedWire client also carries parameters end-to-end
with `QueryWithParams` and depends on the server advertising `FEATURE_PARAMS`.

## Cargo features

- `embedded` (default) — pulls the engine in-process for
  `memory://` / `file:///` URIs.
- `grpc` — async tonic client for `grpc://` / `red://`.
- `http` — REST client over reqwest+rustls for `http://` / `https://`.
- `redwire` — native RedWire TCP client (no engine).
- `redwire-tls` — adds TLS / mTLS to the RedWire transport.

Disable defaults to drop the engine: `default-features = false`.

## `red_client` binary

The crate also hosts the `red_client` binary (built with
`cargo build -p reddb-io-client --bin red_client --no-default-features`).
It is the thin remote-only client used by ops tooling — no
engine, no embedded backend, just transports:

| Scheme              | Status     |
|---------------------|------------|
| `red://host[:port]` | gRPC, default port 5050 |
| `reds://host[:port]`| TODO (TLS not yet wired in the bin) |
| `grpc://host[:port]`| gRPC, default port 5055 |
| `http://host[:port]`| REST |
| `memory:` / `file://` | rejected (exit 2, points to `red`) |

`red_client` is guarded by [`SIZE_BUDGET`](./SIZE_BUDGET) (stripped
release bytes); CI runs `./scripts/check-red-client-size.sh` on
every PR to catch accidental engine re-linkage.

## Module layout

- `crate::Reddb` / `JsonValue` / `ClientError` — published
  high-level API.
- `crate::connect::{Target, parse}` — back-compat shim over
  [`reddb-wire`'s][rw] connection-string parser.
- `crate::connector::{RedDBClient, repl, http, redwire}` —
  workspace-internal connector consumed by the `red` REPL,
  `red_client` bin, and `reddb-server`'s rpc_stdio mode. The
  gRPC connector type itself lives in the
  [`reddb-client-connector`](../reddb-client-connector) sibling
  crate to break a path-dependency cycle.

## References

- [Connection strings][conn-strings]
- [ADR 0001 — RedWire][adr-0001]
- [Workspace migration guide](../../docs/migration/workspace-split.md)

[adr-0001]: ../../.red/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[rw]: ../reddb-wire
[sdk]: ../../docs/clients/sdk-compatibility.md
