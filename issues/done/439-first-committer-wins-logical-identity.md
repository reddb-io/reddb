# First-committer-wins conflicts by logical identity [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/439

Labels: enhancement

GitHub issue number: #439

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Add first-committer-wins conflict detection by logical identity for table-row UPDATE and DELETE under snapshot isolation. Concurrent transactions writing the same logical row should fail deterministically instead of silently losing an update.

## Acceptance criteria

- [ ] Two concurrent transactions updating the same logical row result in one commit and one deterministic conflict error.
- [ ] Concurrent transactions updating different logical rows can both commit.
- [ ] UPDATE-vs-DELETE and DELETE-vs-UPDATE conflicts are detected by logical identity.
- [ ] Autocommit writes participate in the same conflict policy.
- [ ] Conflict tests verify final visible state and error surface.

## Blocked by

- #436
- #438
