# Statement execution and DML contract closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/336

Labels: prd

GitHub issue number: #336

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Prove the statement execution frame, privilege/lock intent derivation, CollectionContract enforcement, and DML Target Scan behavior through public SQL/API outcomes. Split only the missing acceptance criteria into narrower follow-ups.

Covers: #46, #48, #49, #50, #51, #52

User stories covered: 7, 8, 9

## Acceptance criteria

- [ ] Statement execution context is evidenced for read paths with auth, tenant, config, and policy state visible through behavior.
- [ ] Privilege and lock intent derivation is evidenced from public read/write operations, not private implementation names only.
- [ ] CollectionContract enforcement is evidenced for INSERT and mutation paths across relevant collection models.
- [ ] DELETE and UPDATE share observable DML target scan semantics or the missing shared behavior is split into follow-up issues.
- [ ] The evidence report no longer marks #46, #48, #49, #50, #51, or #52 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
