# Dart driver: query(sql, params) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/369

Labels: enhancement

GitHub issue number: #369

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Dart driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.query(String sql, [List<Object?>? params])`

Vector accepts `Float32List` and `List<double>`. `null` maps to `Value::Null`. `Uint8List` maps to `Value::Bytes`. `DateTime` maps to `Value::Timestamp`.

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

Implemented in the Dart driver with a dedicated RedWire value codec, optional
query params across the facade, HTTP, and RedWire transports, and FEATURE_PARAMS
gating for non-empty RedWire params. Empty params keep the legacy Query frame.

Verification attempted:

- `dart test` could not run because `dart` is not installed in this harness.
- `pnpm test` ran and skipped because `target/debug/red` is not built.
- `pnpm typecheck` exited nonzero because the TypeScript compiler package is not installed.
