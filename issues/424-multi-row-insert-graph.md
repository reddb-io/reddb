# Multi-row INSERT VALUES for graph NODE/EDGE [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/424

Labels: needs-triage

GitHub issue number: #424

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

Multi-row `INSERT INTO <coll> NODE (...) VALUES (...), (...), (...)` and equivalent for EDGE. Reduces per-row JSON-RPC overhead drastically.

## Acceptance criteria

- [ ] `INSERT INTO <coll> NODE (col1, col2) VALUES (...), (...)` parses and executes.
- [ ] Same for `EDGE` and for plain row collections (if not already supported).
- [ ] Returns one id per row (per #E1).
- [ ] Atomic semantics: all rows succeed or all fail.
- [ ] Bench: at least 5× speedup over equivalent N single-row inserts via stdio.
