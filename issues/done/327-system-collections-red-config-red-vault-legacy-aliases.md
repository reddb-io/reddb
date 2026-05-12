# System collections red.config/red.vault + legacy aliases [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/327

Labels: enhancement

GitHub issue number: #327

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Bootstrap protected system Config/Vault collections and normalize legacy pseudo-paths onto explicit targets so old references keep working without becoming the canonical model.

## Acceptance criteria

- [ ] Bootstrap creates `red.config` as system Config and `red.vault` as system Vault.
- [ ] Normal users cannot create, drop, truncate, or purge those system collections.
- [ ] Reads/writes require system-scoped capabilities.
- [ ] `$config.*`, `$secret.*`, and `red.secret.*` normalize internally to `config:red.config/<key>` or `vault:red.vault/<key>`.
- [ ] Policy and audit logs always record normalized explicit targets.
- [ ] Docs mark legacy aliases as compatibility only, not the primary API.

## Blocked by

- Blocked by #322
- Blocked by #330
