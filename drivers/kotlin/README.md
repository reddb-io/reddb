# reddb-kotlin

Coroutine-first Kotlin/JVM driver for [RedDB](https://reddb.dev). Speaks the
same RedWire binary protocol and HTTP REST API as the Rust / Go / Java / JS
drivers shipped in this repo.

## Highlights

- **Suspend-fun-first.** Every operation is a `suspend fun`; uses
  `kotlinx-coroutines-core` and `Dispatchers.IO` for non-blocking I/O.
- **Two transports under one `connect()`** — picks RedWire-over-TCP/TLS or
  HTTP/HTTPS automatically from the URL scheme.
- **One conn = one socket.** Concurrent calls serialise through a `Mutex`
  so a single connection always reads exactly one response per request.
- **SCRAM-SHA-256, bearer, anonymous** auth methods; auto-login over HTTP
  when credentials are supplied.

## Usage

```kotlin
import dev.reddb.connect
import dev.reddb.Options
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    connect("red://localhost:5050").use { conn ->
        conn.ping()
        conn.insert("users", mapOf("name" to "alice", "age" to 30))
        val rows = conn.query("SELECT * FROM users WHERE name = 'alice'")
        println(String(rows))
    }
}
```

With credentials:

```kotlin
connect(
    "red://example.com:5050",
    Options(username = "alice", password = "hunter2"),
)
```

HTTP transport with bearer token:

```kotlin
connect("https://reddb.example.com:5050?token=tok-abc")
```

## URL shapes

| URL                                    | Transport      | TLS |
| -------------------------------------- | -------------- | --- |
| `red://host[:port]`                    | RedWire (TCP)  | no  |
| `reds://host[:port]`                   | RedWire        | yes |
| `http://host[:port]`                   | HTTP           | no  |
| `https://host[:port]`                  | HTTP           | yes |
| `red://[user[:pass]@]host[:port]?...`  | any            | —   |

Default port is **5050** for every scheme.

## Building

```sh
./gradlew test
```

The smoke test (`SmokeTest`) is gated on `RED_SMOKE=1` and spawns the
`red serve` binary via `cargo run --release`. Leave it off for ordinary
test runs.

## Layout

```
src/main/kotlin/dev/reddb/
  Reddb.kt              connect() entry point
  Conn.kt               common interface
  Options.kt
  Url.kt                URL parser
  RedDBException.kt     sealed error hierarchy
  redwire/
    Frame.kt            16-byte LE header, MAX 16 MiB
    Codec.kt            zstd-jni reflective bridge
    Scram.kt            SCRAM-SHA-256 client
    RedwireConn.kt      ktor-network socket + TLS
  http/
    HttpConn.kt         ktor-client (CIO), bearer header
```

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
