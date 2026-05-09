# Vault unseal, redaction, audit, and policy surface [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/318

Labels: needs-triage

GitHub issue number: #318

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Ship the Vault operational contract: redacted GET, privileged UNSEAL, versioned ROTATE/HISTORY, tombstone DELETE, privileged PURGE, and mandatory audit.

## Acceptance criteria

- [ ] `GET VAULT` returns metadata and redacted value only.
- [ ] `UNSEAL VAULT` returns plaintext only with `vault:unseal`.
- [ ] `ROTATE VAULT` creates a new version and preserves limited history.
- [ ] `DELETE VAULT` creates a tombstone; `PURGE VAULT` is privileged and audited.
- [ ] Every unseal, purge, key attach, failed unseal, and failed purge emits audit without plaintext/ciphertext.
- [ ] Policy targets use `vault:` capabilities, not `kv:`.

## Blocked by

- #317
