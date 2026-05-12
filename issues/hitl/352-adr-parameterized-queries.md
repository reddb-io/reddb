# ADR: parameterized queries — syntax, Value enum, wire frame, capability negotiation [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/352

Labels: hitl

GitHub issue number: #352

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

ADR `00XX-parameterized-queries.md` codifying the design decisions for parameterized queries across the engine, wire protocols, and all official drivers. This is the contract that every subsequent slice implements against.

The ADR must decide and document:

- Placeholder syntax: `$N` (canonical) and `?` (positional). Mixing in one statement is rejected. Behavior of repeated `$N`. Numbering rules for `?`.
- The engine `Value` enum surface: null, bool, int (i64), float (f64), text, bytes, vector (`Vec<f32>`), json, timestamp (epoch nanos), uuid. Extensibility policy.
- AST: new `Expr::Placeholder(slot)` node; binder rewrites placeholders before execution.
- RedWire wire frame layout for `QueryWithParams` (binary, compact). Capability negotiation per ADR 0001.
- HTTP request body: `{"sql": "...", "params": [...]}`. JSON encoding of non-JSON-native types (`{"$bytes": "..."}`, `{"$ts": ...}`, `{"$uuid": "..."}`).
- Embedded stdio JSON-RPC method shape.
- Error taxonomy: arity mismatch, type mismatch, mixed syntax, gap in `$N`, etc.
- Backwards-compatibility policy: old client + new server, new client + old server.
- Deprecation policy for unsafe string-concat patterns in docs/examples.

## Acceptance criteria

- [ ] ADR file added under `docs/adr/` with the next sequential number.
- [ ] All decisions above are explicitly stated (not deferred).
- [ ] ADR is reviewed and accepted (status: Accepted).
- [ ] PRD #351 references the ADR number.

## Blocked by

None - can start immediately.
