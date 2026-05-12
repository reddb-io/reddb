# C++ driver: query(sql, span<Value>) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/370

Labels: needs-triage

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

- [ ] New `query(sql, params)` overload implemented.
- [ ] Original `query(sql)` signature unchanged.
- [ ] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [ ] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [ ] Integration test covering int/text/null/vector params end-to-end.
- [ ] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357
