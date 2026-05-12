# Dart driver: query(sql, params) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/369

Labels: needs-triage

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

- [ ] New `query(sql, params)` overload implemented.
- [ ] Original `query(sql)` signature unchanged.
- [ ] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [ ] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [ ] Integration test covering int/text/null/vector params end-to-end.
- [ ] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357
