# reddb-go

Pure-Go driver for [RedDB](https://github.com/reddb-io/reddb). Speaks the
RedWire binary TCP protocol, gRPC query API, and HTTP REST surface from a
single facade.

```go
import reddb "github.com/reddb-io/reddb-go"

ctx := context.Background()

// Plain RedWire on the default port (5050).
c, err := reddb.Connect(ctx, "red://localhost:5050")
if err != nil { ... }
defer c.Close()

if err := c.Ping(ctx); err != nil { ... }

row := map[string]any{"name": "alice"}
if err := c.Insert(ctx, "users", row); err != nil { ... }

body, err := c.Query(ctx, "SELECT name FROM users")
result, err := c.Exec(ctx, "INSERT INTO users (id, name) VALUES ($1, $2)", int64(42), "Ada")
affected := result.RowsAffected()
```

## Parameterized queries

`Query` and `Exec` are variadic: pass `$N` bind values after the SQL string.
Native Go types map to the engine's `Value` variants via the wire codec from #357.
With no params the call keeps emitting the legacy `Query` frame byte-for-byte
on RedWire. On gRPC the same typed values are sent in `QueryRequest.params`;
empty params keep that field unset.

```go
// int + text + null
body, err := c.Query(ctx,
  "SELECT * FROM users WHERE age > $1 AND name = $2 AND nick IS $3",
  int64(18), "alice", nil)

// vector similarity ([]float32 → Vector)
embedding := []float32{0.12, -0.45, 0.88 /* … 1536 dims */}
hits, err := c.Query(ctx,
  "SELECT id FROM docs SEARCH SIMILAR embedding TO $1 LIMIT 10",
  embedding)

// bytes + timestamp + uuid
u, _ := redwire.UUIDFromString("550e8400-e29b-41d4-a716-446655440000")
_, err = c.Query(ctx,
  "INSERT INTO blobs (id, body, at) VALUES ($1, $2, $3)",
  u, []byte{0xde, 0xad}, time.Now())

result, err := c.Exec(ctx,
  "INSERT INTO users (id, name) VALUES ($1, $2)",
  int64(42), "Ada")
affected := result.RowsAffected()
```

Native Go type mapping:

| Go type                              | Wire `Value`            |
| ------------------------------------ | ----------------------- |
| `nil`                                | `Null`                  |
| `bool`                               | `Bool`                  |
| `int`, `int8`..`int64`, `uint8`..`uint32`, in-range `uint`/`uint64` | `Int` (i64) |
| `float32`, `float64`                 | `Float` (f64)           |
| `string`                             | `Text` (utf-8)          |
| `[]byte`                             | `Bytes`                 |
| `[]float32`, `[]float64`             | `Vector` (f32 on-wire)  |
| `map[string]any`, `[]any`, `json.RawMessage` | `Json` (canonical) |
| `time.Time`                          | `Timestamp` (unix secs) |
| `redwire.UUID`                       | `Uuid`                  |
| `*T` (any of the above)              | recurses, nil → `Null`  |

If the server didn't advertise `FEATURE_PARAMS` during the handshake the
driver returns `reddb.CodeParamsUnsupported` rather than silently sending
raw `$N` literals.

## Connection strings

| URI                                  | Transport                                    |
| ------------------------------------ | -------------------------------------------- |
| `red://host[:5050]`                  | RedWire (default)                            |
| `reds://host[:5050]`                 | RedWire over TLS (`redwire/1` ALPN)          |
| `grpc://host[:5055]`                 | gRPC query API                               |
| `grpcs://host[:5055]`                | gRPC query API over TLS                      |
| `red://host?proto=grpc`              | RedWire URI, but routed over gRPC            |
| `red://host?proto=https`             | RedWire URI, but routed over HTTPS           |
| `http://host[:8080]`                 | RedDB HTTP REST                              |
| `https://host[:8443]`                | RedDB HTTP REST over TLS                     |
| `red:///path/file.rdb` *(unsupported)* | Embedded — pure-Go driver does not embed   |
| `red://memory` *(unsupported)*       | Embedded in-memory — same                    |

Auth shorthands the URI carries:

- `red://user:pass@host` — SCRAM (RedWire) or `/auth/login` (HTTP).
- `red://host?token=...` — pre-issued bearer.
- `red://host?apiKey=...` — alias for `token` in the URI.

## SDK Helper Spec

This driver implements **SDK Helper Spec v1.0**
([`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md)). The version
is exposed as `reddb.HelperSpecVersion`.

```go
h := reddb.NewHelpers(conn)

// Documents
ins, err := h.Documents().Insert(ctx, "people", map[string]any{"name": "Ada"})
got, err := h.Documents().Get(ctx, "people", ins.RID)
out, err := h.Documents().List(ctx, "people", reddb.ListOptions{Limit: 20})
upd, err := h.Documents().Patch(ctx, "people", ins.RID, map[string]any{"name": "Grace"})
del, err := h.Documents().Delete(ctx, "people", ins.RID) // {Affected, Deleted}

// KV (defaults to collection "kv_default")
err  = h.KV().Set(ctx, "characters:hansel", "ok")
val, err := h.KV().Get(ctx, "characters:hansel") // nil when missing — NOT NotFound
ex,  err := h.KV().Exists(ctx, "characters:hansel")
lst, err := h.KV().List(ctx, reddb.KVListOptions{Prefix: "characters:"})
ttl  := reddb.SetOptions{ExpireMs: 60_000, Tags: []string{"corpus"}}
err  = h.KV().Put(ctx, "k", "v", ttl)

// Queues (h.Queue() and h.Queues() are aliases)
_ = h.Queues().Create(ctx, "jobs") // idempotent CREATE QUEUE IF NOT EXISTS
p := 5
_, err = h.Queues().Push(ctx, "jobs", map[string]any{"id": 1}, reddb.PushOptions{Priority: &p})
peek,_ := h.Queues().Peek(ctx, "jobs", 1)
pop, _ := h.Queues().Pop(ctx, "jobs", 1)
n,   _ := h.Queues().Len(ctx, "jobs")
_, err  = h.Queues().Purge(ctx, "jobs")

// Transactions — imperative + optional Run callback
tx := h.Tx()
_ = tx.Begin(ctx)
// ... conn.Query / conn.Exec ...
_ = tx.Commit(ctx) // or _ = tx.Rollback(ctx)

err = tx.Run(ctx, func(child *reddb.TxClient) error {
    _, err := conn.Exec(ctx, "INSERT INTO t (v) VALUES (1)")
    return err // non-nil → ROLLBACK + return; nil → COMMIT
})
```

### Envelopes

| Envelope            | Fields                                            |
| ------------------- | ------------------------------------------------- |
| `InsertResult`      | `Affected`, `RID`, `Item`                         |
| `DeleteResult`      | `Affected`, `Deleted`                             |
| `ExistsResult`      | `Exists`                                          |
| `ListResult`        | `Items`, `NextCursor`                             |
| `QueuePushResult`   | `Affected`, `RID`                                 |

See [`helpers.go`](helpers.go) for source fields. Validation failures raise
`reddb.CodeInvalidArgument` before any wire call; missing items raise
`reddb.CodeNotFound`. `documents.Delete` / `kv.Delete` of a missing item
return `{Affected: 0, Deleted: false}` — they are **not** errors, per
spec §4.5 / §5.4.

### Conformance matrix (Helper Spec §12)

Every case in the spec table is ported under
[`conformance_test.go`](conformance_test.go) as `TestConformance_<case_id>`
(dots → underscores). The harness needs a real server, so it is gated on the
same env contract as `internal/redserver/`:

```sh
RED_SMOKE=1 RED_BIN=/path/to/red go test -run TestConformance -v ./...
```

| Case ID                                | Status |
| -------------------------------------- | ------ |
| `generic.query.no_params`              | wired |
| `generic.query_with.params`            | wired |
| `generic.insert.rid`                   | wired |
| `generic.bulk_insert.rids`             | wired |
| `generic.delete`                       | wired |
| `documents.crud_nested_patch`          | wired |
| `documents.delete_missing_no_error`    | wired |
| `documents.patch_empty_rejects`        | wired |
| `kv.exact_key_round_trip`              | wired |
| `kv.missing_get_returns_none`          | wired |
| `kv.delete_returns_envelope`           | wired |
| `queues.fifo_peek_pop_len`             | wired |
| `queues.empty_pop_returns_empty`       | wired |
| `queues.purge_resets_len`              | wired |
| `tx.commit_persists`                   | wired |
| `tx.rollback_discards`                 | wired |
| `errors.not_found.document_get`        | wired |
| `wire.probabilistic.hll_round_trip`    | wired (SQL surface; helper provisional) |
| `wire.vectors.sql_round_trip`          | reachable via `conn.Query` — helper provisional in v1.0 |
| `wire.graph.sql_round_trip`            | reachable via `conn.Query` — helper provisional in v1.0 |
| `wire.timeseries.sql_round_trip`       | reachable via `conn.Query` — helper provisional in v1.0 |
| `errors.invalid_argument.empty_sql`    | engine-side error pass-through |

### Transaction support

Imperative-only (`Begin` / `Commit` / `Rollback`) plus the optional `Run`
callback wrapper. Nested `Run` is rejected with `INVALID_ARGUMENT`; use
explicit `SAVEPOINT` statements via `conn.Exec` if you need nested
semantics.

### Out-of-scope helpers in v1.0

`vectors.*`, `graph.*`, `timeseries.*`, `probabilistic.*` namespaces are
**provisional** — reachable via raw `conn.Query()` / `conn.Exec()`. Helper
methods land in v1.1. See spec §8 / §9 / §10 / §11 for the required wire
patterns each driver MUST be able to issue today.

## Auth options

```go
reddb.Connect(ctx, "red://h",
  reddb.WithBasicAuth("alice", "hunter2"),       // SCRAM
)

reddb.Connect(ctx, "red://h", reddb.WithToken("api-key-1"))
reddb.Connect(ctx, "red://h", reddb.WithJWT("eyJ..."))
```

## Layout

| Path                              | Purpose                                                       |
| --------------------------------- | ------------------------------------------------------------- |
| `reddb.go`                        | top-level `Connect` + `Conn` interface (facade)               |
| `url.go` / `url_test.go`          | URI parser shared with the JS driver                          |
| `errors.go`                       | typed `Error` + `IsCode`                                      |
| `grpcx/`                          | minimal gRPC Query client + typed param mapper                 |
| `redwire/frame.go`                | 16-byte header, encode / decode, MAX_FRAME_SIZE               |
| `redwire/codec.go`                | pooled zstd compress / decompress                             |
| `redwire/scram.go`                | RFC 5802 client primitives (HMAC-SHA-256, PBKDF2, proof)      |
| `redwire/conn.go`                 | TCP / TLS dial, handshake state machine, ops                  |
| `httpx/client.go`                 | `net/http` mirror of the JS HTTP client                       |
| `cmd/redgo-smoke/`                | manual smoke runnable against a live server                   |
| `internal/redserver/`             | opt-in end-to-end smoke that spawns the engine binary         |

## Testing

```sh
cd drivers/go
go test ./...
```

The end-to-end engine smoke at `internal/redserver/` is opt-in:

- skipped by default,
- skipped when `RED_SKIP_SMOKE=1`,
- runs only when `RED_SMOKE=1` and `RED_BIN=/path/to/red` are set.

## Not (yet) supported

- Embedded mode (`red:///path` and `red://memory`). The pure-Go build can't
  link the engine; a future cgo build will close that gap.
- gRPC mutations beyond `Query` and `Ping`.
- First-class helpers for `vectors.*`, `graph.*`, `timeseries.*`,
  `probabilistic.*` — provisional per Helper Spec v1.0 (use `conn.Query`).
- `bulk_insert_binary` (0x06) — JSON path is wired; binary fast path is TODO.
- Streaming bulk inserts (`BulkStreamStart` / `BulkStreamRows`).

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
