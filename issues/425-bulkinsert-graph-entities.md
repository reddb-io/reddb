# bulkInsert for graph NODE/EDGE entities across drivers [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/425

Labels: needs-triage

GitHub issue number: #425

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement — driver

## What to build

`db.bulkInsert(collection, rows)` (already in JS SDK for row collections) accepts graph NODE / EDGE rows when the collection is graph-typed. Internally batches via single RPC frame.

Today: 1741 single-row graph INSERTs take ~15s over stdio. Mostly JSON-RPC handshake overhead.

## Acceptance criteria

- [ ] `db.bulkInsert(coll, [{label, name, ...}, ...])` works for NODE rows.
- [ ] Same for EDGE rows: `{label, from, to, ...}`.
- [ ] Returns `{affected, ids: [...]}` matching #E1.
- [ ] Bench: 1000 graph inserts in <2s via stdio (vs ~9s today).
- [ ] Parity across drivers (JS, Python, Go, Rust at minimum).
