# Java driver: query(String sql, Object... params) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/365

Labels: enhancement

GitHub issue number: #365

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Java driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.query(String sql, Object... params)`

JDBC-style. Vector accepts `float[]`. `null` maps to `Value::Null`. `byte[]` maps to `Value::Bytes`. `Instant` maps to `Value::Timestamp`.

## Acceptance criteria

- [x] New `query(sql, params)` overload implemented.
- [x] Original `query(sql)` signature unchanged.
- [x] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [x] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [x] Integration test covering int/text/null/vector params end-to-end.
- [x] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357
