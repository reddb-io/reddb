# ASK exposed as MCP tool [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/409

Labels: needs-triage

GitHub issue number: #409

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK as an MCP tool so LLM agents can ground their answers through the same machinery.

Tool schema: name `reddb.ask`, params `{ question, options? }` where options mirror SQL options (`STRICT`, `USING`, `LIMIT`, `MIN_SCORE`, `DEPTH`, `TEMPERATURE`, `SEED`, `CACHE`, `NOCACHE`). Returns the full ASK response schema.

## Acceptance criteria

- [ ] MCP tool `reddb.ask` registered.
- [ ] Tool description and examples emphasize the grounding/citation guarantee.
- [ ] All ASK options accessible via the tool.
- [ ] Streaming optional via MCP progressive responses.
- [ ] Integration test from an MCP client (or harness) calling the tool.
- [ ] Documented in `docs/api/mcp.md`.

## Blocked by

- #393
