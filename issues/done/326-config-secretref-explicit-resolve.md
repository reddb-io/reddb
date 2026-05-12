# Config SecretRef + explicit resolve [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/326

Labels: enhancement

GitHub issue number: #326

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Allow Config values to reference Vault secrets without embedding plaintext. `GET CONFIG` returns the reference; resolving it is an explicit operation that also performs Vault authorization/audit.

## Acceptance criteria

- [ ] `SECRET_REF(vault, key)` can be stored as a Config value.
- [ ] `GET CONFIG` returns the SecretRef structure, never the referenced plaintext.
- [ ] `RESOLVE CONFIG` or equivalent API resolves the referenced secret only with Config read plus Vault unseal permission.
- [ ] Resolve emits both Config resolve and Vault unseal audit context without plaintext/ciphertext.
- [ ] Broken/missing references return typed errors that do not leak secret material.
- [ ] Tests cover store, get, resolve success, permission denied, and missing target.

## Blocked by

- Blocked by #322
- Blocked by #330
