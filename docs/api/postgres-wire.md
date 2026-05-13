# PostgreSQL Wire Protocol

RedDB speaks PostgreSQL's v3 wire protocol so existing tools — psql,
pgAdmin, DBeaver, the JDBC driver, `pgx`, `asyncpg` — can connect
without any RedDB-specific client.

## Starting the PG listener

```bash
red server --pg-bind 127.0.0.1:5432
```

Or bind alongside gRPC / HTTP:

```bash
red server \
  --bind       127.0.0.1:8080 \
  --grpc-bind  127.0.0.1:5055 \
  --pg-bind    127.0.0.1:5432
```

## Connecting from psql

```bash
psql -h 127.0.0.1 -p 5432 -U reddb reddb
```

Then any supported SQL:

```sql
CREATE TABLE users (id INT, email TEXT);
INSERT INTO users VALUES (1, 'a@b');
SELECT * FROM users;

BEGIN;
SAVEPOINT s1;
DELETE FROM users WHERE id = 1;
ROLLBACK TO SAVEPOINT s1;
COMMIT;
```

## Safe Parameter Binding Status

The PG listener supports PostgreSQL's extended protocol (`Parse` / `Bind` /
`Describe` / `Execute`) for `$N` placeholders. PostgreSQL drivers that send
prepared/parameterized statements can bind numeric, text, bool, bytea, JSON,
UUID, timestamp, and RedDB vector parameters without string concatenation. The
cross-driver binding contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

`ASK` also accepts a bound question over the extended protocol:

```sql
ASK $1::text STRICT OFF LIMIT 5
```

The PG listener returns `ASK` as a normal non-streaming, single-row result set.
Streaming `ASK ... STREAM` is available through HTTP/SSE, not PG-wire.

## From a driver

The examples below use normal driver APIs. Static SQL may use the simple-query
path; parameterized calls use the extended protocol.

### Rust (`tokio-postgres` / `sqlx`)

```rust
let (client, conn) = tokio_postgres::connect(
    "host=localhost port=5432 user=reddb",
    tokio_postgres::NoTls,
).await?;
tokio::spawn(async move { conn.await.unwrap() });

let rows = client
    .query("SELECT id, email FROM users WHERE id = $1", &[&42i32])
    .await?;
```

### Go (`pgx`)

```go
conn, err := pgx.Connect(ctx, "postgres://reddb@localhost:5432/reddb")
rows, err := conn.Query(ctx, "SELECT id, email FROM users WHERE id = $1", 42)
ask, err := conn.Query(ctx, "ASK $1::text STRICT OFF LIMIT 5", "why did login fail?")
```

### Python (`psycopg`)

```python
import psycopg
with psycopg.connect("host=localhost port=5432 user=reddb") as conn:
    with conn.cursor() as cur:
        cur.execute("SELECT id, email FROM users WHERE id = %s", (42,))
        for row in cur.fetchall():
            print(row)
        cur.execute("ASK %s::text STRICT OFF LIMIT 5", ("why did login fail?",), prepare=True)
        print(cur.fetchone())
```

### Java (JDBC)

```java
try (PreparedStatement ps = conn.prepareStatement(
        "ASK ?::text STRICT OFF LIMIT 5")) {
    ps.setString(1, "why did login fail?");
    try (ResultSet rs = ps.executeQuery()) {
        if (rs.next()) {
            System.out.println(rs.getString("answer"));
        }
    }
}
```

## ASK Result Columns

`ASK` over PG-wire always returns one row with the canonical non-streaming ASK
shape:

| Column | PG type/OID | Notes |
|--------|-------------|-------|
| `answer` | text / 25 | Synthesised answer text |
| `cache_hit` | bool / 16 | Whether answer cache supplied the row |
| `citations` | jsonb / 3802 | Citation marker metadata |
| `completion_tokens` | int8 / 20 | Completion token count |
| `cost_usd` | numeric / 1700 | Estimated provider cost |
| `mode` | text / 25 | `strict` or `lenient` |
| `model` | text / 25 | Provider model that answered |
| `prompt_tokens` | int8 / 20 | Prompt token count |
| `provider` | text / 25 | Provider that answered |
| `retry_count` | int8 / 20 | Citation-validation retries |
| `sources_flat` | jsonb / 3802 | Flat source array used by `[^N]` markers |
| `validation` | jsonb / 3802 | Validation result, warnings, and errors |

## Protocol coverage

| Feature | Status |
|---------|--------|
| Startup message + parameter negotiation | ✅ |
| Simple query (`Q` frame) | ✅ |
| RowDescription (`T`) / DataRow (`D`) / CommandComplete (`C`) | ✅ |
| ReadyForQuery (`Z`) | ✅ |
| ErrorResponse (`E`) | ✅ |
| Cleartext password auth | ✅ |
| SSL request (rejected with `N`) | ✅ |
| Extended query (Parse / Bind / Describe / Execute) | ✅ |
| SCRAM-SHA-256 auth | 🟡 Planned |
| TLS-wrapped connection | 🟡 Planned |
| COPY protocol | 🟡 Use `COPY FROM 'file'` instead |

## Type mapping

RedDB values are returned in PG text format under the following OIDs:

| RedDB type | PG OID | Notes |
|------------|--------|-------|
| Integer / UnsignedInteger | 20 (int8) | 64-bit |
| Float | 701 (float8) | 64-bit |
| Boolean | 16 (bool) | `t` / `f` |
| Text | 25 (text) | UTF-8 |
| Json / Blob | 114 (json) / 17 (bytea) | |
| TimestampMs | 1184 (timestamptz) | ms → ISO-8601 |
| Date | 1082 (date) | Unix days → YYYY-MM-DD |
| Vector | 38000 (reddb vector) | RedDB-reserved stable OID |
| NodeRef / EdgeRef | 25 (text) | Serialised string |
| Null | — | NULL bytes |

Inbound bind parameters accept the following PostgreSQL OIDs:

| PG OID | RedDB Value |
|--------|-------------|
| 16 (`bool`) | Boolean |
| 17 (`bytea`) | Blob |
| 20/21/23/26 (`int8`/`int2`/`int4`/`oid`) | Integer |
| 700/701/1700 (`float4`/`float8`/`numeric`) | Float |
| 25/1043/705 (`text`/`varchar`/`unknown`) | Text |
| 114/3802 (`json`/`jsonb`) | Json |
| 1114/1184 (`timestamp`/`timestamptz`) | Timestamp |
| 2950 (`uuid`) | Uuid |
| 38000 (`reddb vector`) | Vector |

Vector OID `38000` is RedDB-reserved. PostgreSQL extension OIDs are normally
cluster-local, so RedDB does not claim pgvector's dynamic catalog OID. Text
binds use JSON vector literals such as `[1.0, 0.0]`; binary binds use the
pgvector-compatible shape `i16 dimensions`, `i16 reserved`, then big-endian
`f32` values.

## Catalog compatibility

PG-wire translates a focused PostgreSQL catalog slice from RedDB's `red.*`
virtual schema so generic clients can inspect tables, columns, and indexes
without RedDB-specific metadata calls.

Supported read-only views:

| PostgreSQL relation | RedDB source |
|---------------------|--------------|
| `information_schema.tables` | `red.collections` |
| `information_schema.columns` | `red.columns` |
| `pg_catalog.pg_tables` | `red.collections` |
| `pg_catalog.pg_indexes` | `red.indices` |
| `pg_catalog.pg_namespace` | synthetic `red` namespace |
| `pg_catalog.pg_class` | `red.collections` + `red.indices` |
| `pg_catalog.pg_attribute` | `red.columns` |

The translator handles simple equality filters on table/schema/column names and
`COUNT(*)` probes. It is intentionally read-only and only runs for `SELECT` or
`WITH` queries that reference the supported catalog relations.

## Session state

Every PG-wire connection gets its own thread context, so session-local
state isolated per connection:

- `BEGIN` / `COMMIT` / `ROLLBACK` are per-connection
- `SET TENANT 'acme'` binds only this session
- `SAVEPOINT` stacks are per-connection

## Limitations

- Binary format output is not yet emitted — everything returns in
  text format.
- No TLS termination on the PG listener itself; put a TLS proxy in
  front (Envoy, nginx) or use gRPC which supports TLS natively.

## See also

- [gRPC API](grpc.md)
- [HTTP API](http.md)
- [Auth Overview](../security/overview.md)
- [Transactions](../query/transactions.md)
