# WATCH, LIST, TAGS semantics for Config and Vault [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/321

Labels: needs-triage

GitHub issue number: #321

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Extend keyed collection observation and metadata operations to Config and Vault without importing normal KV destructive semantics.

## Acceptance criteria

- [ ] `LIST CONFIG/Vault PREFIX` supports pagination.
- [ ] Vault list/watch events contain metadata only: key, version, fingerprint, tags, timestamps, actor, LSN.
- [ ] Config watch may include old/new values according to policy.
- [ ] `TAGS` attach indexed metadata for Config/Vault and are never treated as secret.
- [ ] `INVALIDATE` remains KV-only; Config/Vault require explicit ROTATE/LIST/PURGE flows.
- [ ] Watch/list permission names are separated by `config:` and `vault:` capabilities.

## Blocked by

- #316
- #318
