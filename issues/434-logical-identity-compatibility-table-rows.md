# Add logical identity compatibility for table rows [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/434

Labels: enhancement

GitHub issue number: #434

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Add table-row logical identity compatibility so legacy and new rows can be addressed by stable logical identity while preserving existing physical entity identity. Existing persisted table rows without an explicit logical identity must remain readable and updateable by treating their logical identity as their current physical identity.

## Acceptance criteria

- [ ] New table-row writes carry a stable logical identity separate from physical version identity.
- [ ] Existing persisted rows without logical identity open successfully and behave as logical_id = physical entity id.
- [ ] Table reads, updates, and deletes continue to work for pre-existing data after the compatibility mapping.
- [ ] Tests cover new rows, legacy rows, and mixed datasets.

## Blocked by

- #433
