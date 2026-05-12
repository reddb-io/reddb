# C++ driver

C++17 client for RedDB. Speaks the RedWire binary protocol over TCP (plain or TLS) and the HTTP/HTTPS REST surface. Remote-only — embedded URIs throw `EmbeddedUnsupported`.

- **Source:** [`drivers/cpp/`](https://github.com/reddb-io/reddb/tree/main/drivers/cpp) (CMake; not yet packaged on a registry)
- **Status:** Preview

## Build

```sh
cmake -S drivers/cpp -B drivers/cpp/build -DCMAKE_BUILD_TYPE=Release
cmake --build drivers/cpp/build -j
ctest --test-dir drivers/cpp/build --output-on-failure
```

### Dependencies

| Dependency  | Required? | Notes                                                                                       |
|-------------|-----------|---------------------------------------------------------------------------------------------|
| OpenSSL     | Required  | TLS + SCRAM-SHA-256 (HMAC, PBKDF2, RAND).                                                   |
| zstd        | Optional  | Frame compression. When absent, `COMPRESSED` frames cannot be encoded; receiving one throws `CompressedButNoZstd`. |
| libcurl     | Optional  | HTTP transport. Without it, only `red://` / `reds://` URIs work.                            |
| GoogleTest  | Fetched   | Vendored via `FetchContent` (v1.14.0).                                                      |

## Quickstart

```cpp
#include "reddb/reddb.hpp"

int main() {
    auto conn = reddb::connect("red://localhost:5050");
    auto json = conn->query("SELECT 1");
    conn->close();
}
```

## Authentication

```cpp
reddb::ConnectOptions o;
o.token = "sk-secret";                              // bearer
// — or —
o.username = "alice"; o.password = "hunter2";       // SCRAM-SHA-256 over RedWire
// — or —
o.jwt = "<oauth-jwt>";                              // OAuth-JWT

auto conn = reddb::connect("reds://db.example.com:5050", o);
```

## Connection URIs

| URI                                  | Transport                  |
|--------------------------------------|----------------------------|
| `red://host[:port]`                  | RedWire TCP (default 5050) |
| `reds://host[:port]`                 | RedWire TCP + TLS          |
| `red://host?proto=https`             | HTTPS via libcurl          |
| `http://host[:port]`                 | HTTP via libcurl           |
| `https://host[:port]`                | HTTPS via libcurl          |

Embedded URIs (`red:///path`, `memory://`, `file://`) throw `EmbeddedUnsupported`.

## Production checklist

- Use `reds://` / `https://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/cpp/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/cpp/README.md) — full layout, smoke-test gating, build flags.
