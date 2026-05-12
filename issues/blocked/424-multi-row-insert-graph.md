# Multi-row INSERT VALUES for graph NODE/EDGE [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/424

Labels: enhancement

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

## Progress note - 2026-05-12

Implemented and tested the functional graph slice:

- Multi-row `INSERT ... NODE ... VALUES (...), (...) RETURNING *` executes and returns one `red_entity_id` per inserted row.
- Multi-row `INSERT ... EDGE ... VALUES (...), (...) RETURNING *` executes and returns one `red_entity_id` per inserted row.
- Graph NODE/EDGE rows are validated before write, so validation failures do not leave partial graph entities.
- Graph NODE/EDGE execution now routes through batched storage writes and keeps metadata, preprocessor, context-index, cross-ref, and CDC maintenance.
- Plain row multi-row INSERT was already supported and measured at 28.28x faster than 500 equivalent single-row stdio SQL calls in an ad hoc local run.

Blocked on the graph performance acceptance criterion:

- `500` single-row stdio `INSERT NODE` calls vs one 500-row `INSERT NODE` statement measured `1299.36 ms` vs `395.84 ms`, or `3.28x`.
- Earlier 500-row graph run after the first batch pass measured `3.76x`.
- The required `5x` graph speedup is not yet met; remaining work is likely parser/runtime overhead for large graph SQL batches beyond storage-level batching.
