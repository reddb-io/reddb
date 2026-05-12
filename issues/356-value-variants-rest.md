# Additional Value variants (bool, float, bytes, json, timestamp, uuid) end-to-end [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/356

Labels: enhancement

GitHub issue number: #356

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Round out the `Value` enum with the remaining variants and prove them end-to-end via embedded stdio + JS SDK:

- `Value::Bool`
- `Value::Float` (f64) — including NaN, ±inf, subnormals
- `Value::Bytes` (binary blob)
- `Value::Json` (canonical JSON object/array)
- `Value::Timestamp` (epoch nanoseconds)
- `Value::Uuid`

Each variant gets parser context support in the binder (which clauses accept which types), JSON-RPC encoding (with `{"$bytes": ...}`, `{"$ts": ...}`, `{"$uuid": ...}` envelope per ADR), and JS SDK type mapping (`Uint8Array`, `Date`, native `null`, `boolean`, `number`).

## Acceptance criteria

- [ ] All variants round-trip through embedded stdio JSON-RPC.
- [ ] JS SDK maps native types correctly: `null`, `boolean`, `number` (int vs float distinction documented), `Uint8Array`, `Date`, plain object → json, UUID strings.
- [ ] Boundary values tested: i64::MIN/MAX, f64 NaN/±inf, empty/very long bytes, deeply nested json.
- [ ] Property-based round-trip tests for the wire Value codec (deep module).
- [ ] Binder rejects type mismatches with typed errors per variant.

## Blocked by

- #353
