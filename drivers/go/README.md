# reddb-go

Pure-Go driver for [RedDB](https://github.com/forattini-dev/reddb). Speaks the
RedWire binary TCP protocol and the HTTP REST surface from a single facade.

```go
import reddb "github.com/forattini-dev/reddb-go"

ctx := context.Background()

// Plain RedWire on the default port (5050).
c, err := reddb.Connect(ctx, "red://localhost:5050")
if err != nil { ... }
defer c.Close()

if err := c.Ping(ctx); err != nil { ... }

row := map[string]any{"name": "alice"}
if err := c.Insert(ctx, "users", row); err != nil { ... }

body, err := c.Query(ctx, "SELECT name FROM users")
```

## Connection strings

| URI                                  | Transport                                    |
| ------------------------------------ | -------------------------------------------- |
| `red://host[:5050]`                  | RedWire (default)                            |
| `reds://host[:5050]`                 | RedWire over TLS (`redwire/1` ALPN)          |
| `red://host?proto=https`             | RedWire URI, but routed over HTTPS           |
| `http://host[:8080]`                 | RedDB HTTP REST                              |
| `https://host[:8443]`                | RedDB HTTP REST over TLS                     |
| `red:///path/file.rdb` *(unsupported)* | Embedded — pure-Go driver does not embed   |
| `red://memory` *(unsupported)*       | Embedded in-memory — same                    |

Auth shorthands the URI carries:

- `red://user:pass@host` — SCRAM (RedWire) or `/auth/login` (HTTP).
- `red://host?token=...` — pre-issued bearer.
- `red://host?apiKey=...` — alias for `token` in the URI.

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
