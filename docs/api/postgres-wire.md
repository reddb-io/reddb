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
  --grpc-bind  127.0.0.1:50051 \
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

## From a driver

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
rows, err := conn.Query(ctx, "SELECT id, email FROM users")
```

### Python (`psycopg`)

```python
import psycopg
with psycopg.connect("host=localhost port=5432 user=reddb") as conn:
    with conn.cursor() as cur:
        cur.execute("SELECT id, email FROM users")
        for row in cur.fetchall():
            print(row)
```

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
| Extended query (Parse / Bind / Describe / Execute) | 🟡 Planned |
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
| Vector / NodeRef / EdgeRef | 25 (text) | Serialised string |
| Null | — | NULL bytes |

## Session state

Every PG-wire connection gets its own thread context, so session-local
state isolated per connection:

- `BEGIN` / `COMMIT` / `ROLLBACK` are per-connection
- `SET TENANT 'acme'` binds only this session
- `SAVEPOINT` stacks are per-connection

## Limitations

- Extended query protocol (prepared statements via `Parse`/`Bind`) is
  not wired yet. Clients that use prepared statements should fall
  back to simple query mode or use gRPC.
- Binary format output is not yet emitted — everything returns in
  text format.
- No TLS termination on the PG listener itself; put a TLS proxy in
  front (Envoy, nginx) or use gRPC which supports TLS natively.

## See also

- [gRPC API](grpc.md)
- [HTTP API](http.md)
- [Auth Overview](../security/overview.md)
- [Transactions](../query/transactions.md)
