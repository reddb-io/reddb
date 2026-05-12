# LIMIT and ORDER BY on GRAPH <algorithm> commands [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/422

Labels: needs-triage

GitHub issue number: #422

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

`LIMIT N` and `ORDER BY <metric> [ASC|DESC]` support on every `GRAPH <algorithm>` command:

```sql
GRAPH CENTRALITY tales LIMIT 10
GRAPH COMMUNITY tales ORDER BY size DESC LIMIT 5
GRAPH COMPONENTS tales LIMIT 20
GRAPH SHORTEST_PATH '<a>' TO '<b>' LIMIT 100
```

Today: parse error. `GRAPH CENTRALITY` returns implicit top-100 with no way to control.

## Acceptance criteria

- [ ] `LIMIT N` parses and applies to every documented `GRAPH <algorithm>` clause.
- [ ] `ORDER BY` with the algorithm's natural metric works (e.g. centrality_score, component_size).
- [ ] Default top-K is documented; removed implicit truncation surfaces correctly.
- [ ] Tests for limit cap, order direction, and combined `ORDER BY ... LIMIT`.
