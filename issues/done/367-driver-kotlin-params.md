# Kotlin driver: suspend query(sql, vararg params) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/367

Labels: needs-triage

GitHub issue number: #367

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Kotlin driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `suspend fun query(sql: String, vararg params: Any?)`

Coroutine-friendly. Vector accepts `FloatArray` and `List<Float>`. `null` maps to `Value::Null`. `ByteArray` maps to `Value::Bytes`. `Instant` maps to `Value::Timestamp`.

## Acceptance criteria

- [x] New `query(sql, params)` overload implemented.
- [x] Original `query(sql)` signature unchanged.
- [x] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [x] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [x] Integration test covering int/text/null/vector params end-to-end.
- [x] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357
