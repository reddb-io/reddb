# reddb C++ driver

C++20 client for RedDB. Speaks the RedWire binary protocol over TCP
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
#include <array>

#include "reddb/reddb.hpp"

int main() {
    auto conn = reddb::connect("red://localhost:5050");
    auto json = conn->query("SELECT 1");

    std::array<reddb::Value, 2> params = {
        reddb::Value::int64(30),
        reddb::Value("alice"),
    };
    auto rows = conn->query(
        "SELECT * FROM users WHERE age = $1 AND name = $2",
        params);
    auto one = conn->query("SELECT $1", {reddb::Value::int64(42)});

    std::array<float, 3> embedding = {0.1f, 0.2f, 0.3f};
    std::array<reddb::Value, 1> vector_params = {
        reddb::Value::vector(embedding),
    };
    auto hits = conn->query(
        "SEARCH SIMILAR $1 COLLECTION docs LIMIT 5",
        vector_params);

    conn->close();
}
```

`query(sql)` is unchanged. `query(std::string_view sql,
std::span<const reddb::Value> params)` and
`query(sql, {reddb::Value::int64(42)})` bind positional `$N` placeholders and
use the RedWire `QueryWithParams` frame only when the server advertises
`FEATURE_PARAMS`; otherwise the driver throws `PARAMS_UNSUPPORTED`.
HTTP sends the canonical `/query` body with `query` and adds `params` only
for non-empty parameter spans.

Native C++ parameter mapping:

| C++ value | RedDB Value |
|-----------|-------------|
| `std::nullopt` | null |
| `bool` | bool |
| `reddb::Value::int64(...)`, signed integer / unsigned integer up to `INT64_MAX` | int |
| `float`, `double` | float |
| `std::string`, `std::string_view`, string literal | text |
| `reddb::Value::bytes(std::span<const std::byte>)`, `reddb::Value::bytes(std::span<const uint8_t>)` | bytes |
| `reddb::Value::vector(std::span<const float>)` | vector |
| `reddb::Value::json(std::string_view)` | json |
| `reddb::Value::timestamp_seconds(int64_t)` / `timestamp(time_point)` | timestamp |
| `reddb::Value::uuid("00112233-4455-6677-8899-aabbccddeeff")` | uuid |

### Auth options

```cpp
reddb::ConnectOptions o;
o.token = "sk-secret";              // bearer
// — or —
o.username = "alice"; o.password = "hunter2";  // SCRAM-SHA-256 over RedWire
// — or —
o.jwt = "<oauth-jwt>";

auto conn = reddb::connect("reds://db.example.com:5050", o);
```

### Transport selection

| URI                                  | Transport                |
|--------------------------------------|--------------------------|
| `red://host[:port]`                  | RedWire TCP (default 5050) |
| `reds://host[:port]`                 | RedWire TCP + TLS         |
| `red://host?proto=https`             | HTTPS via libcurl        |
| `http://host[:port]`                 | HTTP via libcurl         |
| `https://host[:port]`                | HTTPS via libcurl        |

## Rich helpers (SDK Helper Spec v0.1)

`reddb::helpers::Helpers` wraps a `Conn*` (or any `IQuerier`) with
typed namespaces — `documents`, `kv`, `queue` — mirroring
`drivers/go/helpers.go`.

```cpp
#include "reddb/helpers.hpp"

auto conn = reddb::connect("red://localhost:5050");
auto h = reddb::helpers::Helpers::of(conn.get());

// Documents
reddb::helpers::JsonObject doc;
doc.emplace("name", reddb::helpers::JsonValue::make_string("alice"));
auto inserted = h.documents().insert("people", doc);

// KV
h.kv().set("characters:hansel", reddb::helpers::JsonValue::make_string("ok"));
auto v = h.kv().get("characters:hansel");

// Queue
h.queue().push("jobs", reddb::helpers::JsonValue::make_string("payload"));
auto out = h.queue().pop("jobs", 1);
```

Typed errors (`reddb::helpers::HelperError::code()`):

| Code | When |
| --- | --- |
| `InvalidArgument` | Bad identifier, negative limit, JSON-pointer patch path |
| `NotFound` | `documents.get` / `documents.patch` on missing rid |
| `InvalidResponse` | Insert response missing `rid` |

Transactions are not exposed — use `conn->query("BEGIN" / "COMMIT" /
"ROLLBACK")` directly.

## Smoke test

`tests/smoke_test.cpp` is gated by `RED_SMOKE=1`. It spawns a server
binary from `RED_BIN` (or `RED_BINARY`) and exercises parameterized RedWire
queries end-to-end. CI does not run it; developers do, after building a real
engine binary.

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
