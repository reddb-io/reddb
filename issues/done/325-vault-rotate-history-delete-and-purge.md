# Vault rotate, history, delete, and purge [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/325

Labels: enhancement

GitHub issue number: #325

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Add Vault lifecycle operations after the read/unseal foundation: versioned rotation, metadata-safe history, tombstone delete, and privileged irreversible purge.

## Acceptance criteria

- [ ] `ROTATE VAULT` creates a new sealed version and updates current metadata/fingerprint.
- [ ] `HISTORY VAULT` lists metadata for retained versions without plaintext/ciphertext.
- [ ] `DELETE VAULT` creates a tombstone version rather than erasing history.
- [ ] `PURGE VAULT` irreversibly removes history only with a stronger purge capability.
- [ ] Unsealing an older version requires a separate stronger capability from current-version unseal.
- [ ] Rotate, delete, purge, and failed purge attempts are audited.

## Blocked by

- Blocked by #330
