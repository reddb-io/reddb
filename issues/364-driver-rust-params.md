# Rust driver: query_with(sql, &[Value]) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/364

Labels: needs-triage

GitHub issue number: #364

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Rust driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.query_with(sql, &[Value])  // with IntoValue trait for ergonomic conversions`

Embedded and remote APIs both get the overload. `IntoValue` covers primitives, `Vec<f32>`, `&[u8]`, `serde_json::Value`, `chrono::DateTime`, `Uuid`.

## Acceptance criteria

- [ ] New `query(sql, params)` overload implemented.
- [ ] Original `query(sql)` signature unchanged.
- [ ] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [ ] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [ ] Integration test covering int/text/null/vector params end-to-end.
- [ ] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357
