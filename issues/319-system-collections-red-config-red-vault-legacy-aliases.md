# System collections: red.config/red.vault + legacy aliases [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/319

Labels: needs-triage

GitHub issue number: #319

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Bootstrap protected system collections for engine-managed config and secrets, and normalize legacy pseudo-paths onto the new explicit model.

## Acceptance criteria

- [ ] Bootstrap creates `red.config` as system Config and `red.vault` as system Vault.
- [ ] Users cannot create/drop/truncate those collections through normal DDL.
- [ ] Reads/writes require system-scoped capabilities.
- [ ] Legacy `$config.*`, `$secret.*`, and `red.secret.*` aliases are normalized internally to explicit Config/Vault targets.
- [ ] Audit/policy logs always record normalized targets such as `config:red.config/key` and `vault:red.vault/key`.

## Blocked by

- #315
- #316
- #318
