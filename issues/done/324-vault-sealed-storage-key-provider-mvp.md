# Vault sealed storage + key provider MVP [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/324

Labels: enhancement

GitHub issue number: #324

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Implement the Vault storage foundation: Vault values are sealed before WAL/page/snapshot persistence, key material can come from the MVP provider path, and restore without key material leaves Vault unavailable rather than exposing or corrupting data.

## Acceptance criteria

- [x] `CREATE VAULT <name>` provisions a Vault collection backed by cluster master key material.
- [x] `CREATE VAULT <name> WITH OWN MASTER KEY` provisions per-vault derived key material.
- [x] Vault writes seal plaintext before WAL/page/snapshot persistence.
- [x] Backups contain sealed blobs plus safe metadata only.
- [x] Restore without compatible key material marks the Vault `sealed_unavailable` while allowing the rest of the database to come up.
- [x] Tests assert that persisted Vault payloads are not plaintext.

## Blocked by

None - #315 is present in this branch (`issues/done/315-keyed-models-catalog-ddl-for-kv-config-and-vault.md`).

## Implementation notes

- Added `CREATE VAULT ... WITH OWN MASTER KEY` parsing and `VAULT PUT/GET` command routing.
- `CREATE VAULT` now requires the real enabled/unsealed Auth Vault key provider, seeds cluster vault key material when needed, and stores per-vault master key material for `WITH OWN MASTER KEY`.
- `VAULT PUT` persists `Value::Secret` ciphertext only; `VAULT GET` returns redacted metadata and reports `sealed_unavailable` when reopened without compatible key material.
- Added a persistent e2e test that verifies the raw database artifacts do not contain the vault plaintext and that no-key reopen still brings the database up.

## Verification

- `CARGO_TARGET_DIR=target/agent-324 cargo check -p reddb-server --message-format=short`
- `CARGO_TARGET_DIR=target/agent-324 cargo test -p reddb-server vault --lib --message-format=short`
- `CARGO_TARGET_DIR=target/agent-324 cargo test --test e2e_vault_sealed_storage --message-format=short`
