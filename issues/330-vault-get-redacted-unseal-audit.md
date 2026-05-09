# Vault GET redacted + UNSEAL + audit [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/330

Labels: needs-triage

GitHub issue number: #330

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Ship the safe Vault read path: normal `GET VAULT` returns redacted metadata only, `UNSEAL VAULT` is an explicit privileged plaintext read, and every unseal path is audited without plaintext or ciphertext.

## Acceptance criteria

- [ ] `GET VAULT` returns key, version, fingerprint, tags, timestamps, and redacted value only.
- [ ] `UNSEAL VAULT` returns plaintext only when the caller has `vault:unseal` on the target.
- [ ] Failed unseal attempts are audited with actor, target, reason, request id, and LSN/context where available.
- [ ] Successful unseal attempts are audited without plaintext/ciphertext.
- [ ] `vault:read_metadata` and `vault:unseal` are separate capabilities.
- [ ] Tests prove GET/list-like metadata surfaces do not include plaintext or ciphertext.

## Blocked by

- Blocked by #324
