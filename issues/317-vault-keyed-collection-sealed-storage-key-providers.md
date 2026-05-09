# Vault keyed collection: sealed storage + key providers [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/317

Labels: needs-triage

GitHub issue number: #317

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Implement the Vault storage contract: secrets are sealed before WAL/page/snapshot persistence and restored as sealed data unless the correct key material is attached.

## Acceptance criteria

- [ ] Vault values are encrypted before WAL/page-cache/snapshot persistence.
- [ ] `CREATE VAULT name` uses cluster master key material.
- [ ] `CREATE VAULT name WITH OWN MASTER KEY` supports a per-vault derived key.
- [ ] Restore without key material leaves Vault in `sealed_unavailable` state rather than failing the whole database.
- [ ] `ATTACH VAULT KEY` enables later unseal/rotate operations.
- [ ] No plaintext or ciphertext is exposed by list/watch/get metadata surfaces.

## Blocked by

- #315
