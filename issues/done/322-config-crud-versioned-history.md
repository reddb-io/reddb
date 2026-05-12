# Config CRUD + versioned history [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/322

Labels: enhancement

GitHub issue number: #322

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Implement Config as a stable keyed collection with end-to-end CRUD and versioned history. This slice proves the parser/runtime/API path for `PUT CONFIG`, `GET CONFIG`, `DELETE CONFIG`, `ROTATE CONFIG`, and `HISTORY CONFIG`, while rejecting KV-only volatility operations.

## Acceptance criteria

- [ ] `PUT CONFIG <collection> <key> = <value>` writes a stable config entry and returns version metadata.
- [ ] `GET CONFIG` returns the current plaintext config value plus version/tags metadata.
- [ ] `ROTATE CONFIG` creates a new version and keeps bounded history.
- [ ] `DELETE CONFIG` creates a tombstone version; `HISTORY CONFIG` shows prior versions and tombstones.
- [ ] TTL/EXPIRE, INCR, DECR, ADD, and destructive INVALIDATE are rejected with `INVALID_OPERATION` before policy/execution.
- [ ] Parser/runtime integration tests cover create, update, rotate, history, tombstone delete, and invalid KV-only operations.

## Blocked by

- Blocked by #315
