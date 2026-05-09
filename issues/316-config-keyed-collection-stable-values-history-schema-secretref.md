# Config keyed collection: stable values, history, schema, SecretRef [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/316

Labels: needs-triage

GitHub issue number: #316

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Ship the stable Config keyed collection contract: typed values, no TTL/counters, versioned writes, history/rollback, and explicit secret references.

## Acceptance criteria

- [ ] `PUT/GET/DELETE/ROTATE/HISTORY CONFIG` work end-to-end.
- [ ] `DELETE CONFIG` creates a tombstone version; `PURGE CONFIG` is explicit and privileged.
- [ ] TTL, INCR, DECR, ADD, and destructive invalidation are rejected for Config.
- [ ] Config values can carry optional type/schema metadata.
- [ ] `SECRET_REF(vault, key)` stores a reference without resolving plaintext.
- [ ] Resolving a SecretRef requires explicit operation plus Vault unseal permission.

## Blocked by

- #315
