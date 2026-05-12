# PHP driver: $db->query(sql, [params]) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/368

Labels: needs-triage

GitHub issue number: #368

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

PHP driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `\$db->query(string \$sql, array \$params = [])`

PDO-style. Vector accepts `array` of floats. `null` maps to `Value::Null`. Binary string with explicit type tag maps to `Value::Bytes`. `DateTimeImmutable` maps to `Value::Timestamp`.

## Acceptance criteria

- [x] New `query(sql, params)` overload implemented.
- [x] Original `query(sql)` signature unchanged.
- [x] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [x] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [x] Integration test covering int/text/null/vector params end-to-end.
- [x] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357

## Progress note

Implemented PHP parameterized queries after #357 landed:

- `Conn::query(string $sql, array $params = [])` keeps the single-argument path unchanged.
- HTTP sends typed `/query` JSON params.
- RedWire gates non-empty params on `FEATURE_PARAMS` and emits `QueryWithParams` frame `0x28`.
- `Reddb\Value` provides explicit wrappers for bytes, json, and uuid.
- `Redwire\ValueCodec` pins the Value tag table and binary payload shape for null, bool, int, float, text, bytes, vector, json, timestamp, and uuid.
- Tests cover the codec, RedWire frame path, unsupported-server guard, and gated real-engine smoke coverage for int/text/null/vector params.

Verification notes:

- `git diff --check` passed.
- `pnpm test` ran but skipped because `target/debug/red` is not built.
- `pnpm typecheck` is unavailable in this repo root (`ERR_PNPM_NO_SCRIPT`).
- PHP tests could not be executed in this environment because `php`, `composer`, and `drivers/php/vendor` are absent.
