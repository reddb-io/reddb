# C++ driver

C++20 client for RedDB. Speaks the RedWire binary protocol over TCP (plain or TLS) and the HTTP/HTTPS REST surface. Remote-only — embedded URIs throw `EmbeddedUnsupported`.

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
#include <array>

#include "reddb/reddb.hpp"

int main() {
    auto conn = reddb::connect("red://localhost:5050");

    std::array<reddb::Value, 2> params = {
        reddb::Value(30),
        reddb::Value("alice"),
    };
    auto rows = conn->query(
        "SELECT * FROM users WHERE age = $1 AND name = $2",
        params);

    conn->close();
}
```

## Safe parameter binding

`query(std::string_view sql, std::span<const reddb::Value> params)` binds
positional `$N` placeholders. Use it for user input and vectors instead of
interpolating values into SQL strings. The parameterized-query design is
tracked in [ADR #352](https://github.com/reddb-io/reddb/issues/352).

```cpp
#include <array>
#include <optional>

#include "reddb/reddb.hpp"

auto conn = reddb::connect("red://localhost:5050");

std::array<reddb::Value, 3> scalar_params = {
    reddb::Value(42),
    reddb::Value("acme"),
    reddb::Value(std::nullopt),
};
auto rows = conn->query(
    "SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
    scalar_params);

std::array<float, 3> embedding = {0.1f, 0.2f, 0.3f};
std::array<reddb::Value, 1> vector_params = {
    reddb::Value::vector(embedding),
};
auto hits = conn->query(
    "SEARCH SIMILAR $1 IN embeddings K 5",
    vector_params);
```

Native C++ parameter mapping:

| C++ value | Engine value |
|-----------|--------------|
| `std::nullopt` | Null |
| `bool` | Bool |
| signed integer / unsigned integer up to `INT64_MAX` | Int |
| `float`, `double` | Float |
| `std::string`, `std::string_view`, string literal | Text |
| `reddb::Value::bytes(std::span<const std::byte>)` | Bytes |
| `reddb::Value::vector(std::span<const float>)` | Vector |
| `reddb::Value::json(std::string_view)` | Json |
| `reddb::Value::timestamp_seconds(int64_t)` / `timestamp(time_point)` | Timestamp |
| `reddb::Value::uuid("00112233-4455-6677-8899-aabbccddeeff")` | Uuid |

`query(sql)` with no params stays on the legacy single-query path. RedWire
parameterized queries require the server to advertise `FEATURE_PARAMS`; older
servers raise `ErrorCode::ParamsUnsupported` (`PARAMS_UNSUPPORTED`) instead of
silently dropping params. HTTP sends typed params through `/query` only for
non-empty parameter spans.

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
