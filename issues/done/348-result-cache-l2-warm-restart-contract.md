# Result-cache L2 warm restart contract [AFK]

GitHub: local follow-up from reddb-io/reddb#339 / #147

Labels: enhancement, ready-for-agent

GitHub issue number: #348

## Parent

#339 (https://github.com/reddb-io/reddb/issues/339)

## What to build

Implement or explicitly reject SQL result-cache warm restart through Blob Cache
L2. Current adapter stores only a Blob Cache fingerprint plus an in-memory
`RuntimeQueryResult` sidecar, so a runtime restart can verify presence but
cannot reconstruct the cached result payload from durable L2.

Covers: remaining warm-restart acceptance from #147

## Acceptance criteria

- [x] Eligible result-cache entries can be served after runtime restart from Blob Cache L2, or the unsupported contract is documented as intentionally out of scope.
- [x] Tenant and auth identity isolation remain intact across restart.
- [x] Volatile and transaction-unsafe statements still do not persist into the result cache.
- [x] Table dependency invalidation remains correct before and after restart.
- [x] Expired result entries do not rehydrate.
- [x] Public runtime tests cover the implemented or explicitly rejected warm-restart contract.

## Blocked by

None.
