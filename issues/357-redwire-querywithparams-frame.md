# RedWire QueryWithParams frame + capability negotiation (JS over TCP) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/357

Labels: needs-triage

GitHub issue number: #357

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

New RedWire frame `QueryWithParams` carrying `(sql: String, params: Vec<Value>)` in the compact binary encoding defined by the ADR. End-to-end tracer: JS SDK over TCP RedWire issues a parameterized query, server binds and executes, results return.

Includes:

- Wire Value codec (deep module): pure encode/decode, round-trip testable, used by both client and server.
- Capability negotiation per ADR 0001 — old `Query` frame stays untouched; new clients advertise support, old clients keep working.
- Server: routes new frame to the same binder used by embedded stdio (#353).
- JS SDK `redwire.js`: emits `QueryWithParams` when params are present, falls back to `Query` when empty.

## Acceptance criteria

- [ ] `QueryWithParams` frame defined in `crates/reddb-wire` with versioning.
- [ ] Wire Value codec round-trips every Value variant (property-based tests).
- [ ] Capability negotiation: server advertises support; client checks before sending.
- [ ] New client + old server: clear error or graceful fallback per ADR.
- [ ] Old client + new server: existing `Query` frame still works unchanged.
- [ ] JS SDK over TCP RedWire passes the same parameterized integration suite as embedded stdio.

## Blocked by

- #353
