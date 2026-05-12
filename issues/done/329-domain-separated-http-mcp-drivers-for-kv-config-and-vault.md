# Domain-separated HTTP/MCP/drivers for KV, Config, and Vault [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/329

Labels: enhancement

GitHub issue number: #329

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Expose separate public surfaces for normal KV, Config, and Vault across HTTP, MCP, and official drivers while keeping the internal keyed-operation plumbing shared where appropriate.

## Acceptance criteria

- [ ] HTTP exposes separate `/v1/kv`, `/v1/config`, and `/v1/vault` routes.
- [ ] MCP tools are separated as `reddb_kv_*`, `reddb_config_*`, and `reddb_vault_*`.
- [ ] Drivers expose separate clients such as `db.kv()`, `db.config()`, and `db.vault()`.
- [ ] Vault `get` returns redacted metadata; Vault `unseal` is explicit in every transport/driver.
- [ ] Config SecretRef resolution is explicit and requires Vault permission.
- [ ] Transport/driver tests prove Config/Vault reject TTL/counter operations consistently.

## Blocked by

- Blocked by #322
- Blocked by #330
- Blocked by #326
