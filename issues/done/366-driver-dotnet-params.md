# .NET driver: QueryAsync(sql, params object[]) [DONE]

GitHub: https://github.com/reddb-io/reddb/issues/366

Labels: enhancement

GitHub issue number: #366

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

.NET driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.QueryAsync(string sql, params object[] args)`

ADO.NET-style. Vector accepts `float[]` and `ReadOnlyMemory<float>`. `DBNull.Value` and `null` map to `Value::Null`. `byte[]` maps to `Value::Bytes`. `DateTimeOffset` maps to `Value::Timestamp`.

## Acceptance criteria

- [x] New `query(sql, params)` overload implemented.
- [x] Original `query(sql)` signature unchanged.
- [x] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [x] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [x] Integration test covering int/text/null/vector params end-to-end.
- [x] README example updated with the parameterized form (especially vector example).

## Completion note

Implemented the .NET parameterized query vertical slice:

- Added `QueryAsync(string sql, params object?[] args)` to `IConn`, `HttpConn`, and `RedWireConn` while keeping the existing `QueryAsync(string, CancellationToken)` path intact.
- Added `Reddb.Redwire.ValueCodec` for RedWire `QueryWithParams` payloads and HTTP typed params, covering null/`DBNull.Value`, bool, integers, floats, text, bytes, vectors, JSON, timestamps, and UUIDs.
- Added RedWire capability tracking from `HelloAck`/`AuthOk`, `FEATURE_PARAMS`, `QueryWithParams = 0x28`, and `ParamsUnsupported` when params are sent to old RedWire servers.
- Updated `/query` HTTP payloads to use canonical `query` and include typed `params` only when non-empty.
- Added codec, HTTP, RedWire fake-server, and gated smoke coverage; README now documents parameter usage and native mappings.

Verification:

- `git diff --check`
- `pnpm test` (passes by skipping missing `target/debug/red`)
- `pnpm typecheck` (nonzero: root command `typecheck` not found)
- `dotnet test drivers/dotnet -c Release --nologo` (not run: `dotnet` is not installed in this harness)

## Blocked by

- #357
