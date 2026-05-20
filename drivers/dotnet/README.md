# Reddb (.NET driver)

Official .NET driver for [RedDB](https://github.com/reddb/reddb). Speaks
the **RedWire** binary protocol (TCP + TLS) and the JSON HTTP API.

* Target framework: `net8.0`
* NuGet package id: `Reddb`
* License: MIT

## Install

```bash
dotnet add package Reddb
```

## Quick start

```csharp
using Reddb;

await using IConn conn = await Reddb.ConnectAsync("red://localhost:5050");

await conn.InsertAsync("users", new { name = "alice", age = 30 });

ReadOnlyMemory<byte> rows = await conn.QueryAsync("SELECT * FROM users");
string json = System.Text.Encoding.UTF8.GetString(rows.Span);

ReadOnlyMemory<byte> filtered = await conn.QueryAsync(
    "SELECT * FROM users WHERE age = $1 AND name = $2",
    30,
    "alice");

JsonNode? envelope = await conn.QueryAsync<JsonNode>(
    "SELECT * FROM users WHERE age = $1",
    30);

ReadOnlyMemory<byte> nearest = await conn.QueryAsync(
    "SELECT * FROM docs ORDER BY embedding <-> $1 LIMIT 3",
    new float[] { 0.12f, 0.34f, 0.56f });
```

## Query parameters

`QueryAsync(string sql)` is unchanged. Use
`QueryAsync(string sql, params object?[] args)` to bind positional `$N`
placeholders:

```csharp
await conn.QueryAsync("SELECT * FROM users WHERE id = $1", 42);
```

Native mappings:

| .NET value                                      | RedDB value      |
| ----------------------------------------------- | ---------------- |
| `sbyte`, `byte`, `short`, `ushort`, `int`, `uint`, `long` | integer |
| `float`, `double`                               | float            |
| `bool`                                          | bool             |
| `null`, `DBNull.Value`                          | null             |
| `string`                                        | text             |
| `byte[]`                                        | bytes            |
| `float[]`, `ReadOnlyMemory<float>`              | vector           |
| `JsonNode`, `JsonElement`, dictionaries, lists, arrays | json      |
| `DateTime`, `DateTimeOffset`                    | timestamp        |
| `Guid`                                          | uuid             |

## Rich helpers (SDK Helper Spec v1.0)

`Reddb.Helpers.Helpers` wraps an `IConn` with the four first-class
namespaces from the canonical
[`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md): `Documents()`,
`Kv()`, `Queues()` (alias: `Queue()`), and `Tx()`. The helper API mirrors
the Go / Rust / Python helpers.

`Helpers.HelperSpecVersion` is `"1.0"` — cross-driver CI dashboards
assert against this constant per spec §14.

```csharp
using Reddb.Helpers;

var helpers = Helpers.For(conn);

// Documents
var ins = await helpers.Documents().InsertAsync("people",
    new Dictionary<string, object?> { ["name"] = "alice" });
var row = await helpers.Documents().GetAsync("people", ins.Rid);
var patched = await helpers.Documents().PatchAsync("people", ins.Rid,
    new Dictionary<string, object?> { ["name"] = "alicia" });
DeleteResult del = await helpers.Documents().DeleteAsync("people", ins.Rid);
// del.Deleted is true when del.Affected > 0; missing rid → {0, false}, NOT NOT_FOUND.

// KV (default collection: kv_default)
await helpers.Kv().SetAsync("characters:hansel", "witch");
object? v = await helpers.Kv().GetAsync("characters:hansel"); // null when missing, NOT NOT_FOUND
var list = await helpers.Kv().ListAsync(new KvClient.ListOpts { Prefix = "characters:" });

// Queues
await helpers.Queues().CreateAsync("jobs"); // idempotent (CREATE QUEUE IF NOT EXISTS)
await helpers.Queues().PushAsync("jobs",
    new Dictionary<string, object?> { ["id"] = 1 },
    new QueueClient.PushOptions { Priority = 5 });
long len = await helpers.Queues().LenAsync("jobs");
var jobs = await helpers.Queues().PopAsync("jobs", 10);

// Transactions — imperative + callback
var tx = helpers.Tx();
await tx.BeginAsync();
await conn.QueryAsync("INSERT INTO people (name) VALUES ('eve')");
await tx.CommitAsync();

await tx.RunAsync(async _ =>
{
    await conn.QueryAsync("INSERT INTO people (name) VALUES ('frank')");
}); // commits on success, rolls back + rethrows on exception
```

### Envelope summary

| Envelope          | Fields                                                |
| ----------------- | ----------------------------------------------------- |
| `InsertResult`    | `Affected` (always 1), `Rid`, `Item`                  |
| `DeleteResult`    | `Affected`, `Deleted` (`Affected > 0`)                |
| `ExistsResult`    | `Exists`                                              |
| `ListResult`      | `Items`, optional `NextCursor`                        |
| `QueuePushResult` | `Affected`, `Rid`                                     |

Typed errors: `HelperException.InvalidArgument`,
`HelperException.NotFound`, `HelperException.InvalidResponse`.

### Transaction support

Both imperative (`BeginAsync` / `CommitAsync` / `RollbackAsync`) and
callback (`RunAsync(async tx => …)`) forms. Nested `RunAsync` rejects
with `INVALID_ARGUMENT` — call `conn.QueryAsync("SAVEPOINT …")`
directly for savepoint semantics (spec §7.2).

### Conformance matrix

The .NET driver ports every case ID from
[`docs/spec/sdk-helpers.md` §12](../../docs/spec/sdk-helpers.md). Run
the harness against a real engine:

```bash
RED_SMOKE=1 RED_BIN=/path/to/red dotnet test drivers/dotnet -c Release \
    --filter "FullyQualifiedName~Reddb.Tests.ConformanceTests"
```

| Case ID                              | Status |
| ------------------------------------ | ------ |
| `generic.query.no_params`            | ✅     |
| `generic.query_with.params`          | ✅     |
| `generic.insert.rid`                 | ✅     |
| `generic.bulk_insert.rids`           | reachable via `conn.BulkInsertAsync` |
| `generic.delete`                     | ✅     |
| `documents.crud_nested_patch`        | ✅     |
| `documents.delete_missing_no_error`  | ✅     |
| `documents.patch_empty_rejects`      | ✅     |
| `kv.exact_key_round_trip`            | ✅     |
| `kv.missing_get_returns_none`        | ✅     |
| `kv.delete_returns_envelope`         | ✅     |
| `queues.fifo_peek_pop_len`           | ✅     |
| `queues.empty_pop_returns_empty`     | ✅     |
| `queues.purge_resets_len`            | ✅     |
| `tx.commit_persists`                 | ✅     |
| `tx.rollback_discards`               | ✅     |
| `errors.not_found.document_get`      | ✅     |
| `wire.probabilistic.hll_round_trip`  | ✅     |
| `wire.vectors.sql_round_trip`        | reachable via `conn.QueryAsync` (spec §8 provisional)  |
| `wire.graph.sql_round_trip`          | reachable via `conn.QueryAsync` (spec §9 provisional)  |
| `wire.timeseries.sql_round_trip`     | reachable via `conn.QueryAsync` (spec §10 provisional) |

### Out of scope (v1.0)

- `vectors.*`, `graph.*`, `timeseries.*`, `probabilistic.*` first-class
  helpers — provisional namespaces; reach today via `conn.QueryAsync`.
  Lifted into helpers in v1.x per spec §8–§11.
- KV TTL helpers (`kv.expire(…)`) — use `WITH TTL` on the underlying
  `KV PUT` until v1.1.
- Isolation-level argument on `tx.begin` — engine default only in v1.0.

## Supported URIs

One URI string covers every transport:

| URI                                   | Transport            |
| ------------------------------------- | -------------------- |
| `red://host[:port]`                   | RedWire (TCP)        |
| `reds://host[:port]`                  | RedWire over TLS     |
| `http://host[:port]`                  | HTTP REST            |
| `https://host[:port]`                 | HTTPS REST           |
| `red://user:pass@host`                | with SCRAM-SHA-256   |
| `red://host?token=sk-abc`             | bearer token         |
| `red://host?apiKey=ak-xyz`            | API key              |

The default port is `5050` for every scheme — matches the engine's
`DEFAULT_REDWIRE_PORT`.

## Auth

* `ConnectOptions.Token` → bearer auth on RedWire and HTTP.
* `ConnectOptions.Username` + `Password` → SCRAM-SHA-256 on RedWire,
  auto `/auth/login` on HTTP.
* No credentials → anonymous (the server must allow it).

## Tests

```bash
dotnet restore drivers/dotnet
dotnet build drivers/dotnet -c Release
dotnet test drivers/dotnet -c Release --nologo
```

Smoke tests are gated on `RED_SMOKE=1` and spawn the real engine via
`cargo run`. Set `RED_BIN=/path/to/red` to reuse an existing binary.

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
