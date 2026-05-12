# Config schema and type validation [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/323

Labels: enhancement

GitHub issue number: #323

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Add optional type/schema validation for Config entries so stable settings can reject invalid values before rollout. This extends the Config CRUD path from #322 without changing Vault or normal KV semantics.

## Acceptance criteria

- [x] Config entries can declare or attach expected value types such as bool, int, string, url, object, or array.
- [x] `PUT CONFIG` and `ROTATE CONFIG` reject values that do not match the declared type/schema.
- [x] Schema/type metadata is visible in Config metadata/history.
- [x] Schema changes are versioned or audited so operators can explain why a later value was accepted/rejected.
- [x] Normal KV remains schemaless and Vault remains opaque except for content metadata.

## Blocked by

- Blocked by #322
