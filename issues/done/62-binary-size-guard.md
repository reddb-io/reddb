# CI binary-size guard for red_client [AFK]

GitHub issue: reddb-io/reddb#62
Parent PRD: reddb-io/reddb#54
Blocked by: #60

CI check fails when released `red_client` exceeds documented size threshold. Threshold set after first successful build measures baseline + recorded in CI config or repo-tracked file. Guards against accidental engine re-linkage.

## Acceptance Criteria
- [ ] CI step measures stripped release size of `red_client`
- [ ] Threshold documented (CI config or tracked file) reflecting baseline
- [ ] CI fails when binary exceeds threshold
- [ ] Threshold change requires PR
- [ ] Runs on every PR

## Feedback Loops
- CI workflow validation
