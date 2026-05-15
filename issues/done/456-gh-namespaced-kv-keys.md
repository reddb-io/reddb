# Support namespaced KV keys across SQL, HTTP, and SDK surfaces [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/456

Labels: enhancement, ready-for-agent

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#449

## What to build

Make namespaced KV keys containing `:` work consistently across public surfaces. SQL/DSL should require quoting for special-character keys; HTTP should use normal URL encoding; SDK helpers should accept plain strings.

## Acceptance criteria

- [ ] `INSERT INTO <collection> KV (key, value) VALUES ('characters:hansel', ...)` stores the exact key.
- [ ] `KV GET 'characters:hansel'` and `KV DELETE 'characters:hansel'` work without normalization or corruption.
- [ ] HTTP KV endpoints accept URL-encoded namespaced keys such as `characters%3Ahansel`.
- [ ] SDK KV helpers accept `characters:hansel` as a normal string key.
- [ ] Unquoted special-character DSL forms fail with a helpful error that suggests quoting.
- [ ] Tests cover SQL/DSL, HTTP, persistence, and at least one SDK conformance path.

## Blocked by

- #451

