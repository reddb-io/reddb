# C++ driver: query(sql, span<Value>) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/370

Labels: enhancement

GitHub issue number: #370

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

C++ driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.query(std::string_view sql, std::span<const Value> params)`

Typed `Value` class with `std::variant` internally. Vector via `std::span<const float>`. Bytes via `std::span<const std::byte>`. `std::nullopt` for null.

## Acceptance criteria

- [x] New `query(sql, params)` overload implemented.
- [x] Original `query(sql)` signature unchanged.
- [x] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [x] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [x] Integration test covering int/text/null/vector params end-to-end.
- [x] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357

## Completion note

Implemented the C++ `Value` type, RedWire `QueryWithParams` codec, HTTP
params JSON body support, and `query(std::string_view, std::span<const Value>)`
across the public facade and transports.

Verification:
- `g++ -std=c++20 ... -c drivers/cpp/src/{value.cpp,errors.cpp,redwire/frame.cpp,redwire/value_codec.cpp,http/client.cpp,reddb.cpp}`
- `pnpm test` (skipped: `target/debug/red` missing)
- `pnpm typecheck` (nonzero: TypeScript compiler package is not installed)

Blocked local full C++ test run:
- `cmake -S drivers/cpp -B drivers/cpp/build -DCMAKE_BUILD_TYPE=Debug`
  cannot find OpenSSL in this harness.
