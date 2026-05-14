# Docs: "What works in RedDB 1.0.x SQL" reference page [AFK]

Labels: docs, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

A single reference page enumerating every supported SQL clause, operator, builtin function, and DDL form in RedDB 1.0.x. The feedback's "Documentation gaps" section lists a dozen items the user had to learn by probing (KV's `:` rule, that `bulkInsert` is single-row over the wire, that `CACHE` is HTTP-only, the full GRAPH algorithm list, etc.). Each of those resolves into one row in this reference.

The page is generated where possible (lexer keyword table + parser dispatch table) and maintained by hand where not (semantic notes on JOIN flavors, isolation levels, mode-detection rules).

## Acceptance criteria

- [ ] Doc page lives under the existing docs tree and is linked from the SDK README.
- [ ] Sections: SELECT clauses (WHERE, GROUP BY, ORDER BY, LIMIT, OFFSET, JOIN flavors, subqueries), DML (INSERT / UPDATE / DELETE / RETURNING), DDL (every CREATE / ALTER / DROP form), GRAPH commands, KV commands, QUEUE commands, TIMESERIES, probabilistic, transactions, EXPLAIN.
- [ ] Each entry names: the syntax, a one-line example, and the status (supported / partial / not yet).
- [ ] Builtin functions are listed with signatures and return types (`NOW()`, `CURRENT_TIMESTAMP`, `UPPER`, `LOWER`, `COALESCE`, `CASE WHEN ... END`, aggregates, etc.).
- [ ] Operators list includes `||` (concat), arithmetic, comparison, `IN`, `BETWEEN`, `LIKE`, `IS NULL`.
- [ ] Mode-detection rules section documents what triggers each query mode (SQL / SPARQL / Cypher / Natural / Gremlin / Path).
- [ ] Where a section's content can be derived from the lexer keyword table or parser dispatch table, a generation script lives in `scripts/` and runs in CI to keep the doc in sync.
- [ ] Every feature shipped by issues #446–#465 and #467–#478 is reflected in the page.

## Blocked by

- #467 (mode detector)
- #468 (parameterised binder)
- #469 (SELECT-led JOIN)
- #470 (subquery in expression)
- #471 (concat fix)
- #472 (CURRENT_TIMESTAMP)
- #473 (DESCRIBE)
- #474 (SHOW CREATE TABLE)
- #475 (SHOW INDEXES)
- #476 (default column names)
- #477 (RETURNING)
- #478 (SDK transaction wrapper)
