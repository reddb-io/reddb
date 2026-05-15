# Close probabilistic public command and SQL-read contract [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/455

Labels: enhancement, ready-for-agent

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#449

## What to build

Make probabilistic structures reliable through both documented commands and SQL-read forms. Command forms remain the primary UX, while SQL-read forms are public secondary UX for dashboards, views, and SDK query workflows.

## Acceptance criteria

- [ ] `HLL COUNT`, `SKETCH COUNT`, and `FILTER CHECK` work through the documented command syntax.
- [ ] `SELECT CARDINALITY FROM <hll>` returns a stable cardinality column and respects aliases.
- [ ] `SELECT FREQ('x') AS freq FROM <sketch>` returns a stable frequency/estimate value and respects aliases.
- [ ] `SELECT CONTAINS('x') AS hit FROM <filter>` returns a stable boolean value and respects aliases.
- [ ] Unsupported SQL forms such as `SELECT * FROM <probabilistic>` produce helpful errors pointing to the correct API.
- [ ] HTTP `/query` covers command forms and SQL-read forms.
- [ ] Persistence/reopen tests cover HLL, sketch, and filter state.

## Blocked by

- #451

