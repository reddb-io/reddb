# ADR 0015 - Parameterized query contract

**Status:** Draft (open for human review)
**Date:** 2026-05-13
**Supersedes:** -
**Superseded by:** -
**Related ADRs:** [0001 - Redwire TCP protocol](0001-redwire-tcp-protocol.md),
[0010 - serialization boundary discipline](0010-serialization-boundary-discipline.md)
**Related issues:** [#352 - ADR: parameterized queries](https://github.com/reddb-io/reddb/issues/352),
[#377 - ADR: parameter contract, wire Value enum, error taxonomy](https://github.com/reddb-io/reddb/issues/377)
**Parent issue:** [#351 - PRD: Parameterized queries and prepared statements](https://github.com/reddb-io/reddb/issues/351)

## Context

RedDB accepts parameterized queries through embedded Rust, HTTP, MCP, stdio,
gRPC, RedWire, and external language drivers. Those paths must agree on the
same parameter value vocabulary, placeholder syntax, bind-error contract, and
transport encoding. Without one recorded contract, drivers can pass local tests
while disagreeing on edge cases such as bytes, UUIDs, timestamps, vector
parameters, or placeholder arity.

The existing implementation already has a shared engine binder
(`user_params::bind`), RedWire `QueryWithParams` frame, gRPC `QueryValue`, JSON
`params` arrays, and shared golden fixtures in
`crates/reddb-wire/tests/fixtures/params/manifest.json`. This ADR records that
current contract so future parameter and prepared-statement work extends one
surface instead of creating transport-specific dialects.

This ADR remains Draft until the human HITL gates on issues #352 and #377
accept it.

## Decision

### 1. Placeholder syntax

RedDB supports positional placeholders only:

- `$1`, `$2`, ..., `$N` are explicit one-based slots.
- `?` is an ordered positional slot, numbered left-to-right starting at slot 1.
- A single statement must not mix `$N` and `?`.
- `$0`, negative slots, gaps in `$N` slots, and arity mismatches are bind
  errors.
- Named placeholders are not part of this contract. A future named-parameter
  design must lower names to this positional contract before binding.

The AST stores parameter slots as zero-based indices so it can index directly
into the supplied parameter array. The parser owns placeholder recognition and
placeholder-style mixing errors. The binder owns contiguous-slot and arity
validation after the full statement shape is known.

Drivers surface the same contract as ordered values:

| Driver / surface | Parameter API |
| --- | --- |
| Embedded Rust | `query_with(sql, &[Value])` |
| HTTP Rust client | `query_with(sql, &[Value])`, encoded as JSON `params` |
| gRPC Rust client | `query_with(sql, &[Value])`, encoded as typed `QueryValue` |
| JS / TS | `db.query(sql, paramsArray)` |
| Bun | `db.query(sql, paramsArray)` |
| Python | `db.query(sql, *params)` or `db.query(sql, params=[...])` |
| Go | `Query(ctx, sql, params ...any)` |
| Java | `query(String sql, Object... params)` |
| .NET | `QueryAsync(sql, params object[] params)` |
| Kotlin | `query(sql, vararg params)` |
| PHP | `query(string $sql, array $params = [])` |
| Dart | `query(sql, params: [...])` |
| C++ | `query(sql, std::span<const reddb::Value> params)` or `query(sql, {Value::int64(42)})` |
| Zig | `query(sql, .{...})` or `query(sql, []const Value)` |
| CLI / MCP / stdio | JSON `params` array alongside `sql`; CLI also accepts repeatable `-p` / `--param` |

Driver documentation may prefer `$N` examples because it is stable across
query rewrites and easier to reason about in generated SQL, but `?` remains a
valid positional form where the parser accepts expression or parameter slots.

### 2. Parameter Value taxonomy

The parameter Value enum is deliberately smaller than the storage schema value
space. Every transport must preserve these variants:

| Variant | Meaning |
| --- | --- |
| `Null` | SQL null / missing value |
| `Bool` | Boolean |
| `Int64` | Signed 64-bit integer |
| `Float64` | IEEE 754 double |
| `Text` | UTF-8 string |
| `Blob` / `Bytes` | Opaque bytes |
| `Vector` | Ordered `f32` vector for vector slots |
| `Json` | JSON object, array, string, number, boolean, or null |
| `Timestamp` | Unix timestamp represented as `i64` |
| `Uuid` | 16-byte UUID |

Transport-specific type names can differ (`Bytes` in code, `Blob` in docs),
but they map to this same value. Storage-only schema variants must not leak
into driver codecs unless a future ADR adds them to the parameter Value enum.

### 3. Error taxonomy

Parameterized query failures use stable error codes plus transport-native
status wrappers:

| Code / status | Shape | When it is used |
| --- | --- | --- |
| `INVALID_PARAMS` | `{ code, message }` | Missing or malformed method arguments, non-array `params`, binder arity/gap/type mismatch, unsupported parameter value type |
| `QUERY_ERROR` | `{ code, message }` | SQL parse or execution errors after the method arguments are valid |
| `PARAM_COUNT_OVER_LIMIT` | `{ code, message }` | RedWire/client codec receives more than the maximum supported parameter count |
| `PARAMS_UNSUPPORTED` | `{ code, message }` | RedWire driver attempts a parameterized query against a server that did not advertise `FEATURE_PARAMS` |
| gRPC `INVALID_ARGUMENT` | `Status::invalid_argument(message)` | gRPC parameter conversion, parse, or bind failure |
| HTTP 400 | JSON error body | HTTP parse, conversion, or bind failure |

Binder messages are part of the debugging contract but not a localization
contract. Current canonical bind messages are:

| Binder case | Message shape |
| --- | --- |
| Wrong arity | `wrong number of parameters: SQL expects {expected}, got {got}` |
| Gap | `parameter ${missing} is missing (max index used is ${max}) - $N indices must be contiguous starting at $1` |
| Unsupported shape | `this query shape does not support $N parameters in the tracer-bullet slice` |
| Type mismatch | `parameter type mismatch: {slot} (got {got})` |

Transports may prefix parse or bind messages with local context, for example
gRPC `parse error: ...` or `bind error: ...`, but must preserve enough detail
to distinguish parse failure, arity/gap, unsupported type, and unsupported
server capability.

### 4. Transport encoding

Each transport must choose one of two serialization styles: typed values or
JSON-encoded values.

| Transport | Encoding |
| --- | --- |
| Embedded Rust | Typed in-process conversion from client `Value` to engine `SchemaValue`; no serialization round trip |
| RedWire | Typed binary `QueryWithParams` frame `0x28`, gated by `FEATURE_PARAMS`; value tags `0x00..0x09` map to the Value taxonomy above |
| gRPC | Typed `QueryRequest.params: repeated QueryValue` |
| HTTP `/query` | JSON body `{ "query": sql, "params": [...] }` |
| MCP / stdio JSON-RPC | JSON `params` array in the tool/RPC argument object |
| CLI | JSON parameter arguments forwarded to HTTP/MCP/stdio-compatible shapes |

JSON transports use natural JSON values for null, booleans, numbers, strings,
arrays, and objects. Typed values that JSON cannot represent unambiguously use
single-key envelopes:

| JSON envelope | Value |
| --- | --- |
| `{ "$bytes": "<base64>" }` | `Blob` / `Bytes` |
| `{ "$ts": <integer-or-string> }` | `Timestamp` |
| `{ "$uuid": "<hyphenated-uuid>" }` | `Uuid` |
| numeric array where a vector slot is expected | `Vector` |
| other array/object | `Json` |

RedWire encodes the SQL text plus parameter count followed by typed values in
the shared fixture layout `redwire-query-with-params-v1`. gRPC encodes the same
variants through protobuf oneofs. Both typed transports must round-trip the
fixture manifest without losing variant identity.

Postgres-wire extended query protocol is tracked separately. When that surface
maps PostgreSQL OIDs and bind formats into RedDB parameters, it must lower into
this same Value taxonomy before reaching the engine binder.

### 5. Conformance

`crates/reddb-wire/tests/fixtures/params/manifest.json` is the canonical
cross-driver fixture set for parameter encoding. A driver adding parameter
support must either consume the manifest directly in tests or add an equivalent
fixture check that proves the same SQL, parameter list, and wire bytes.

Engine-level behavior remains covered by binder and transport tests; the
fixture manifest guards codec compatibility across RedWire, gRPC, and external
drivers.

## Consequences

- Positional arrays are the only accepted public driver surface for now.
- Drivers can add ergonomic local conversions, but they must collapse to the
  shared Value enum before crossing a transport boundary.
- JSON transports are convenient and inspectable, but typed transports remain
  the authority for preserving bytes, UUID, timestamp, vector, and JSON
  distinctions.
- Named parameters and full PostgreSQL extended-protocol parity are explicitly
  future work, not implicit behavior in the current contract.
- Issues #352 and #377 can be closed only after human review changes this ADR
  status from Draft to Accepted or records an equivalent acceptance decision.
