# Vault GET redacted + UNSEAL + audit [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/330

Labels: enhancement

GitHub issue number: #330

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Ship the safe Vault read path: normal `GET VAULT` returns redacted metadata only, `UNSEAL VAULT` is an explicit privileged plaintext read, and every unseal path is audited without plaintext or ciphertext.

## Acceptance criteria

- [x] `GET VAULT` returns key, version, fingerprint, tags, timestamps, and redacted value only.
- [x] `UNSEAL VAULT` returns plaintext only when the caller has `vault:unseal` on the target.
- [x] Failed unseal attempts are audited with actor, target, reason, request id, and LSN/context where available.
- [x] Successful unseal attempts are audited without plaintext/ciphertext.
- [x] `vault:read_metadata` and `vault:unseal` are separate capabilities.
- [x] Tests prove GET/list-like metadata surfaces do not include plaintext or ciphertext.

## Blocked by

- Blocked by #324

## Completion note

Implemented on top of #324 sealed Vault storage.

- `GET VAULT` now emits redacted metadata only: collection, key, version, fingerprint, tags, timestamps, redacted value, and status.
- `UNSEAL VAULT <collection.key>` and `VAULT UNSEAL <collection.key>` are explicit plaintext reads.
- IAM-enabled runtimes enforce separate `vault:read_metadata` and `vault:unseal` actions on `vault:<collection.key>`.
- Embedded/no-IAM runtimes follow the existing local runtime pattern and do not pretend a real capability checker exists; this limitation is explicit here rather than hidden as simulated security.
- Unseal success, denial, not-found, and decrypt failures are audited without plaintext or ciphertext.
- Added focused parser and e2e coverage proving metadata surfaces do not expose plaintext, ciphertext, or `Secret(...)` debug output.

Checks run:

- `CARGO_TARGET_DIR=target/agent-330 cargo test --test e2e_vault_sealed_storage --message-format=short`
- `CARGO_TARGET_DIR=target/agent-330 cargo test -p reddb-server vault_unseal --lib --message-format=short`
- `CARGO_TARGET_DIR=target/agent-330 cargo test -p reddb-server test_parse_unseal_vault_command --lib --message-format=short`
- `CARGO_TARGET_DIR=target/agent-330 cargo check -p reddb-server --message-format=short`
