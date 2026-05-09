# Domain-separated transports and drivers for KV, Config, and Vault [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/320

Labels: needs-triage

GitHub issue number: #320

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Expose separate public API and driver surfaces for normal KV, Config, and Vault while sharing internal keyed-operation plumbing.

## Acceptance criteria

- [ ] HTTP exposes `/v1/kv`, `/v1/config`, and `/v1/vault` domain routes.
- [ ] MCP tools are separated as `reddb_kv_*`, `reddb_config_*`, and `reddb_vault_*`.
- [ ] Drivers expose `db.kv()`, `db.config()`, and `db.vault()` clients.
- [ ] Vault `get` returns redacted metadata; Vault `unseal` is explicit.
- [ ] Config SecretRef resolution is explicit and requires Vault permission.
- [ ] Transport tests prove invalid Config/Vault TTL/counter operations are rejected consistently.

## Blocked by

- #316
- #318
