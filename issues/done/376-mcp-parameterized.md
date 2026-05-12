# MCP: expose parameterized queries via MCP tool [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/376

Labels: enhancement

GitHub issue number: #376

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Expose parameterized queries through the MCP (Model Context Protocol) server (`docs/api/mcp.md`). The MCP query tool accepts a `params` field so LLM agents producing queries do so safely by construction.

Today the MCP query tool only takes a SQL string — an LLM that wants to filter by user input is forced to interpolate the value, which is both unsafe and brittle.

## Acceptance criteria

- [ ] MCP query tool schema extended with optional `params` array.
- [ ] Tool description and examples emphasize the parameterized form for any user-provided value.
- [ ] Server dispatches MCP-supplied params through the same binder as other transports.
- [ ] Integration test issuing a parameterized query via MCP.
- [ ] `docs/api/mcp.md` updated.

## Blocked by

- #358
