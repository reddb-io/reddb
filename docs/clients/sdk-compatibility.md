# SDK & Client Compatibility

PLAN.md Phase 10.4. RedDB exposes four transports — PostgreSQL wire (v3), gRPC, HTTP, and a native binary wire protocol. This page lists which clients work out of the box, what SQL feature subset each transport carries, and where the rough edges still are.

## Transport matrix

| Transport | Default port | Bind env | Auth |
|-----------|--------------|----------|------|
| PostgreSQL wire | none (off by default) | `--pg-bind` flag | password / token |
| gRPC | `:50051` | `RED_GRPC_BIND_ADDR` / `--grpc-bind` | bearer (per RPC) |
| HTTP | `:8080` | `RED_HTTP_BIND_ADDR` / `--http-bind` | bearer (`Authorization: Bearer`) |
| Native wire | `:5050` | `--wire-bind` | length-framed binary; bearer optional |

`/admin/*` and `/metrics` always live on the HTTP transport. Operators can split them onto dedicated listeners via `RED_ADMIN_BIND` and `RED_METRICS_BIND` (see [scaling.md](../operations/scaling.md)).

## PostgreSQL wire protocol

Stable enough that off-the-shelf PostgreSQL clients connect and run common workloads. Kept under `--pg-bind` rather than enabled by default because Postgres parity is an evolving target.

### Verified clients

| Client | Status | Notes |
|--------|--------|-------|
| `psql` | ✅ | Connects, executes SELECT/INSERT/UPDATE/DELETE/CREATE TABLE. `\d`, `\dt`, `\du` introspection works against the catalog views RedDB exposes. |
| JDBC (`org.postgresql:postgresql`) | ✅ | Connection pool clients (HikariCP) + ORMs (Hibernate, JOOQ) work for the supported SQL subset. |
| DBeaver | ✅ | Pick the PostgreSQL driver. Schema browser shows tables and indexes. |
| `sqlx` (Rust) | ✅ | `Pool::connect("postgres://…")`. Macros that depend on type-checked queries against a *real* Postgres at compile-time should use `sqlx::query!` only with a Postgres dev DB and run the Postgres binary against RedDB at runtime. |
| `node-postgres` (`pg`) | ✅ | Tested with simple/extended query mode. |
| `psycopg2` / `psycopg` (Python) | ✅ | Same notes as JDBC. |
| pgAdmin | ⚠️ | Connects; some catalog queries (event triggers, replication slots) hit unimplemented features and warn. Browse + execute SQL works. |

### SQL feature subset over PG wire

| Feature | Status |
|---------|--------|
| `CREATE / DROP TABLE`, `ALTER TABLE` | ✅ |
| `CREATE INDEX`, `DROP INDEX` | ✅ |
| `INSERT` / `UPDATE` / `DELETE` | ✅ |
| `SELECT` with `WHERE`, `ORDER BY`, `LIMIT`, `OFFSET` | ✅ |
| `JOIN` (INNER, LEFT) | ✅ |
| `GROUP BY` + aggregations (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`) | ✅ |
| `EXPLAIN` plan tree | ✅ |
| `EXPLAIN ANALYZE` | ✅ (counters: `actual_rows`, `actual_ms`) |
| Prepared statements (`PREPARE` / `EXECUTE`) | ✅ |
| Cursors (`DECLARE` / `FETCH`) | ✅ |
| Transactions (`BEGIN` / `COMMIT` / `ROLLBACK`) | ✅ |
| Stored procedures, triggers | ❌ |
| `LISTEN` / `NOTIFY` | ❌ |
| Logical replication slots over PG protocol | ❌ — use the gRPC `pull_wal_records` RPC + ack pattern instead |
| FDW, extensions | ❌ |
| Window functions | ❌ (post-v1) |

If your application relies on a feature in the second list, prefer the gRPC or HTTP transport — those expose first-class APIs (vector search, graph traversal, time-series).

## gRPC

Schema in [`proto/reddb.proto`](../../proto/reddb.proto). Code-gen with `tonic-build` for Rust or your favourite gRPC compiler for Go/Java/Python/etc.

- Every public RPC accepts a JSON payload via `JsonPayloadRequest` to keep the API forward-compatible without proto bumps for every new field.
- Bearer token via `Authorization` metadata header.
- Long-running RPCs (`pull_wal_records`) honour client-side cancel via the standard tonic deadline.

### Replication client

A correct replica client implements:

1. `register_replica` (or implicit registration by setting `replica_id` on `pull_wal_records`).
2. Loop: `pull_wal_records(since_lsn, max_count, replica_id)` → apply each record in LSN order via your own apply path → `ack_replica_lsn(replica_id, applied_lsn, durable_lsn)`.
3. On disconnect: reconnect with the persisted `last_applied_lsn`; the apply path's stateful applier (Phase 11.5) catches gaps / divergences automatically.

## HTTP

The HTTP transport carries:

- **Data plane**: `/collections/{name}/...` for CRUD, `/query` for SQL, `/changes` for CDC poll.
- **Admin/control plane**: see [`admin-api.openapi.yaml`](../spec/admin-api.openapi.yaml).

Generate clients with the OpenAPI spec served at `GET /admin/openapi.yaml`:

```bash
curl http://<host>:<port>/admin/openapi.yaml > admin.openapi.yaml
openapi-generator-cli generate -i admin.openapi.yaml -g go -o ./reddb-admin-go
```

## Native binary wire

Length-framed binary protocol used for the highest-throughput path (point lookups, bulk insert). Schema documented in [`docs/wire-protocol.md`](../wire-protocol.md). Use only when:

- You're embedding RedDB and want zero-copy point reads.
- You wrote a client in Rust / C / a language with strong byte-buffer ergonomics.
- You're OK with manual upgrades on schema bumps.

Most workloads should pick HTTP or PG wire instead.

## Versioning Promise

| Surface | Stability |
|---------|-----------|
| HTTP `/admin/*` v1 | Stable across patch + minor releases. Major bumps documented in release notes + spec major. |
| HTTP `/metrics` exposition | Additive only within v1. Removed metrics get one release of overlap. |
| gRPC proto | Field numbers + message names stable; new fields are additive. |
| PG wire protocol | Tracks PostgreSQL v3. New SQL features land additively. |
| Native wire | More volatile pre-v1.0; embedded users should pin engine version. |

## Reporting Compatibility Bugs

If a verified client breaks against a release, file an issue with:

- Client name + version.
- Connection string (redact secrets).
- A `pg_dump --schema-only` (or equivalent) of the failing schema.
- The exact failing query.
- `red doctor --json` output from the running server.
