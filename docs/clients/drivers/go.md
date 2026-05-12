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
