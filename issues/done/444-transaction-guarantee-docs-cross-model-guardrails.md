# Transaction guarantee docs and cross-model guardrails [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/444

Labels: enhancement

GitHub issue number: #444

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Update transaction documentation and cross-model guardrails so RedDB's MVCC guarantees are stated exactly: table rows support the new history-store contract, unsupported stronger isolation is not implied, and non-table models either route through the resolver where supported or keep their documented behavior without silent overclaiming.

## Acceptance criteria

- [ ] Transaction docs describe snapshot isolation, versioned UPDATE, tombstone DELETE, conflict behavior, commit recovery boundaries, and manual vacuum requirements.
- [ ] Docs explicitly state that serializable isolation, autovacuum, historical indexes, and full multi-model rollout are out of scope for this slice.
- [ ] Cross-model paths are audited so they either use the shared MVCC resolver where supported or clearly retain existing documented behavior.
- [ ] Tests or docs-conformance checks prevent table-row guarantees from being claimed for unsupported models.
- [ ] The PRD and ADR remain linked from the documentation where appropriate.

## Blocked by

- #437
- #441
- #442
- #443
