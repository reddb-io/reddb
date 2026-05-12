# DDL auth evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/344

Labels: enhancement

GitHub issue number: #344

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Prove destructive DDL authorization for DROP and TRUNCATE through public SQL/API behavior. Denials must occur before execution and allowed principals must execute successfully.

Covers: #309

User stories covered: 12

## Acceptance criteria

- [ ] DROP requires the correct policy action and denied principals cannot mutate state.
- [ ] TRUNCATE requires the correct policy action and denied principals cannot mutate state.
- [ ] Allowed principals can execute the same operations successfully.
- [ ] The evidence report no longer marks #309 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
