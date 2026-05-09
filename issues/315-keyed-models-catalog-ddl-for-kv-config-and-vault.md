# Keyed models: catalog + DDL for KV, Config, and Vault [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/315

Labels: needs-triage

GitHub issue number: #315

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Introduce `Config` and `Vault` as first-class Collection models beside normal `Kv`. Add the DDL and introspection path that makes the distinction visible before runtime operations land.

## Acceptance criteria

- [ ] Catalog can represent `Kv`, `Config`, and `Vault` as distinct models.
- [ ] Parser accepts `CREATE KV`, `CREATE CONFIG`, and `CREATE VAULT`.
- [ ] Parser accepts `DROP KV`, `DROP CONFIG`, and `DROP VAULT` with model-aware validation.
- [ ] `SHOW KVS`, `SHOW CONFIGS`, and `SHOW VAULTS` filter by model.
- [ ] Invalid model operations return `INVALID_OPERATION` before policy/execution.
- [ ] Existing normal KV behavior remains compatible.

## Blocked by

None - can start immediately
