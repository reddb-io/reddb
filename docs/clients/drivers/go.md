# Go driver

Pure-Go client for RedDB — speaks the RedWire binary TCP protocol and the HTTP REST surface from a single facade. No cgo.

- **Module:** `github.com/reddb-io/reddb-go`
- **Source:** [`drivers/go/`](https://github.com/reddb-io/reddb/tree/main/drivers/go)
- **Status:** Preview

## Install

```bash
go get github.com/reddb-io/reddb-go@latest
```

## Quickstart

```go
package main

import (
    "context"
    "log"

    reddb "github.com/reddb-io/reddb-go"
)

func main() {
    ctx := context.Background()

    c, err := reddb.Connect(ctx, "red://localhost:5050")
    if err != nil {
        log.Fatal(err)
    }
    defer c.Close()

    if err := c.Ping(ctx); err != nil {
        log.Fatal(err)
    }

    if err := c.Insert(ctx, "users", map[string]any{"name": "alice"}); err != nil {
        log.Fatal(err)
    }

    body, err := c.Query(ctx, "SELECT name FROM users")
    if err != nil {
        log.Fatal(err)
    }
    log.Printf("rows: %s", body)
}
```

## Connection strings

| URI                                  | Transport                              |
|--------------------------------------|----------------------------------------|
| `red://host[:5050]`                  | RedWire (default port 5050)            |
| `reds://host[:5050]`                 | RedWire over TLS (`redwire/1` ALPN)    |
| `red://host?proto=https`             | RedWire URI, routed over HTTPS         |
| `http://host[:8080]`                 | HTTP REST                              |
| `https://host[:8443]`                | HTTPS REST                             |
| `red:///path/file.rdb` *(unsupported)* | Embedded — pure-Go can't embed       |
| `red://memory` *(unsupported)*       | Embedded in-memory — same              |

URI-carried auth: `red://user:pass@host` (SCRAM / `/auth/login`), `red://host?token=…`, `red://host?apiKey=…`.

## Safe parameter binding

`Query` accepts positional `$N` bind values as variadic `any`. Use it for any
user-supplied value — concatenation is a SQL-injection footgun:

```go
// Scalar params: int / text / null
rows, err := c.Query(ctx,
    "SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
    int64(42), "acme", nil)

// Vector param (HNSW / IVF similarity search):
rows, err = c.Query(ctx,
    "SEARCH SIMILAR $1 IN embeddings K 5",
    []float32{0.1, 0.2, 0.3})
```

Native Go → engine type mapping (see `redwire.EncodeValue`):

| Go                         | Engine            |
|----------------------------|-------------------|
| `nil`                      | Null              |
| `bool`                     | Bool              |
| `intN`, `uintN`            | Int (i64)         |
| `float32`, `float64`       | Float (f64)       |
| `string`                   | Text              |
| `[]byte`                   | Bytes             |
| `[]float32`, `[]float64`   | Vector (f32)      |
| `time.Time`                | Timestamp (secs)  |
| `redwire.UUID`             | Uuid              |
| `map[string]any`, `[]any`  | Json (canonical)  |

RedWire routes through the binary `QueryWithParams` frame (`0x28`) when the
server advertises `FEATURE_PARAMS`; older servers raise `PARAMS_UNSUPPORTED`
instead of silently dropping the params. HTTP forwards a typed `params`
array — `[]byte` ships as `{"$bytes": "<base64>"}`, `time.Time` as
`{"$ts": <unix-seconds>}`, `redwire.UUID` as `{"$uuid": "…"}`.
`Query(ctx, sql)` with no params stays byte-identical to the legacy path —
old servers and the embedded fast path don't see the new frame at all.

## Authentication options

```go
reddb.Connect(ctx, "red://host",
    reddb.WithBasicAuth("alice", "hunter2"))   // SCRAM

reddb.Connect(ctx, "red://host", reddb.WithToken("api-key-1"))
reddb.Connect(ctx, "red://host", reddb.WithJWT("eyJ..."))
```

## Not yet supported

- Embedded mode (`red:///path`, `red://memory`) — pure-Go can't link the engine. A future cgo build will close the gap.
- `bulk_insert_binary` (opcode `0x06`) fast path — JSON path is wired; binary TODO.
- Streaming bulk inserts (`BulkStreamStart` / `BulkStreamRows`).

## Production checklist

- Use `reds://` (or `https://`) for cross-network traffic.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/go/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/go/README.md) — layout, smoke tests, opt-in end-to-end harness.
