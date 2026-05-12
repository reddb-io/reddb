# Config/Vault WATCH, LIST, and TAGS metadata semantics [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/328

Labels: enhancement

GitHub issue number: #328

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Add observation and metadata operations for Config and Vault without importing normal KV destructive semantics. Vault events and lists are metadata-only; Config events may expose values according to policy.

## Acceptance criteria

- [ ] `LIST CONFIG <collection> PREFIX ...` supports pagination and returns values/metadata according to policy.
- [ ] `LIST VAULT <collection> PREFIX ...` supports pagination and returns metadata only.
- [ ] `WATCH CONFIG` emits config changes with old/new values only when policy allows.
- [ ] `WATCH VAULT` emits key/version/fingerprint/tags/actor/LSN metadata only.
- [ ] `TAGS` are indexed metadata for Config/Vault and are never treated as secret.
- [ ] `INVALIDATE` remains KV-only; Config/Vault require explicit rotate/list/purge flows.

## Blocked by

- Blocked by #322
- Blocked by #330
