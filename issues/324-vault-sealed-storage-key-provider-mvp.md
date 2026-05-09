# Vault sealed storage + key provider MVP [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/324

Labels: needs-triage

GitHub issue number: #324

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Implement the Vault storage foundation: Vault values are sealed before WAL/page/snapshot persistence, key material can come from the MVP provider path, and restore without key material leaves Vault unavailable rather than exposing or corrupting data.

## Acceptance criteria

- [ ] `CREATE VAULT <name>` provisions a Vault collection backed by cluster master key material.
- [ ] `CREATE VAULT <name> WITH OWN MASTER KEY` provisions per-vault derived key material.
- [ ] Vault writes seal plaintext before WAL/page/snapshot persistence.
- [ ] Backups contain sealed blobs plus safe metadata only.
- [ ] Restore without compatible key material marks the Vault `sealed_unavailable` while allowing the rest of the database to come up.
- [ ] Tests assert that persisted Vault payloads are not plaintext.

## Blocked by

- Blocked by #315
