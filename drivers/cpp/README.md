# reddb C++ driver

C++17 client for RedDB. Speaks the RedWire binary protocol over TCP
(plain or TLS) and the HTTP/HTTPS REST surface. Remote-only — embedded
URIs (`red:///path`, `memory://`, `file://`) throw
`EmbeddedUnsupported`.

## Build

```sh
cmake -S drivers/cpp -B drivers/cpp/build -DCMAKE_BUILD_TYPE=Release
cmake --build drivers/cpp/build -j
ctest --test-dir drivers/cpp/build --output-on-failure
```

### Dependencies

| Dependency | Required? | Notes |
|------------|-----------|-------|
| OpenSSL    | Required  | TLS + SCRAM-SHA-256 (HMAC, PBKDF2, RAND) |
| zstd       | Optional  | Frame compression. When absent, `COMPRESSED` frames cannot be encoded; receiving one throws `CompressedButNoZstd`. |
| libcurl    | Optional  | HTTP transport. Without it, only `red://` / `reds://` URIs work. |
| GoogleTest | Fetched   | Vendored via `FetchContent` (v1.14.0). |

## Quickstart

```cpp
#include "reddb/reddb.hpp"

int main() {
    auto conn = reddb::connect("red://localhost:5050");
    auto json = conn->query("SELECT 1");
    conn->close();
}
```

### Auth options

```cpp
reddb::ConnectOptions o;
o.token = "sk-secret";              // bearer
// — or —
o.username = "alice"; o.password = "hunter2";  // SCRAM-SHA-256 over RedWire
// — or —
o.jwt = "<oauth-jwt>";

auto conn = reddb::connect("reds://db.example.com:5051", o);
```

### Transport selection

| URI                                  | Transport                |
|--------------------------------------|--------------------------|
| `red://host[:port]`                  | RedWire TCP (default 5050) |
| `reds://host[:port]`                 | RedWire TCP + TLS         |
| `red://host?proto=https`             | HTTPS via libcurl        |
| `http://host[:port]`                 | HTTP via libcurl         |
| `https://host[:port]`                | HTTPS via libcurl        |

## Smoke test

`tests/smoke_test.cpp` is gated by `RED_SMOKE=1`. It spawns a server
binary (`RED_BINARY`, default `target/debug/reddb`) and exercises the
public ops end-to-end. CI does not run it; developers do, after `cargo
build` against a real engine.

## Layout

```
drivers/cpp/
  include/reddb/
    reddb.hpp      url.hpp  errors.hpp
    redwire/{frame,codec,scram,conn}.hpp
    http/client.hpp
  src/
    reddb.cpp  url.cpp  errors.cpp
    redwire/{frame,codec,scram,conn}.cpp
    http/client.cpp
  tests/
    url_test.cpp  scram_test.cpp  frame_test.cpp
    redwire_conn_test.cpp  smoke_test.cpp
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
