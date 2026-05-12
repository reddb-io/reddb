# Docs sweep: ASK guides + API references with new schema [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/412

Labels: enhancement

GitHub issue number: #412

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Sweep the docs so the new ASK citation schema is the default shown to new users.

Pages in scope:
- `docs/guides/ask-your-database.md` — every example shows citations + URNs
- `docs/guides/rag-in-20-lines.md`
- `docs/api/http.md`, `docs/api/embedded.md`, `docs/api/postgres-wire.md`, `docs/api/mcp.md`
- `docs/getting-started/quick-start.md`

Add a 'Grounding and citations' section to the JS/TS driver guide. Cross-link to the ADR (#392).

## Acceptance criteria

- [ ] All listed docs updated to show the new citation schema.
- [ ] No ASK example in docs uses the legacy bucketed-only response.
- [ ] Cross-links to ADR (#392) and to the wedge narrative.
- [ ] Updated guide includes a worked example with `STRICT`, `STREAM`, `CACHE TTL`, and `USING`.

## Blocked by

- #405
- #408
- #409

## Progress

2026-05-12: Confirmed blocked before editing docs. The pages in scope
must document `STREAM`, Postgres-wire ASK, and MCP ASK as user-facing
surfaces, but #405, #408, and #409 are still open. #408 also remains
blocked on HITL #360 for PG-wire extended protocol. Deferring the docs
sweep avoids presenting unimplemented transport behavior as available.
