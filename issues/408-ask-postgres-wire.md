# ASK via Postgres-wire (non-stream) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/408

Labels: needs-triage

GitHub issue number: #408

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK through Postgres-wire so existing psycopg / pgx / JDBC apps can call `ASK` without rewriting.

ASK over PG-wire is non-streaming (PG protocol does not naturally support SSE-style streaming for query results). Response rows expose the ASK fields as columns of a single-row result set: `answer text, sources_flat jsonb, citations jsonb, validation jsonb, provider text, model text, cost_usd numeric`, etc.

Depends on #360 (PG-wire extended protocol) for parameterized question text.

## Acceptance criteria

- [ ] `ASK '...'` executable via psycopg returns a single-row result with the documented columns.
- [ ] Same via pgx (Go).
- [ ] Same via JDBC (Java).
- [ ] No fallback to simple-query mode required.
- [ ] Integration tests against all three clients.
- [ ] Documented in `docs/api/postgres-wire.md` with examples.

## Blocked by

- #393
- #360
