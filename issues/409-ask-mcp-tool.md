# ASK exposed as MCP tool [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/409

Labels: enhancement

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

## Progress

Slice 1: `McpAskTool` deep module landed at
`crates/reddb-server/src/runtime/ai/mcp_ask_tool.rs` with 34 unit
tests. Pure — no I/O, no transport, no MCP server plumbing. Mirrors
the slice-1 pattern of #395, #396, #398, #400, #401, #402, #403,
#405, #411.

Exposes:

- `descriptor() -> Value` — full MCP tool record with `name`,
  `description`, `inputSchema` (JSON-Schema draft-07).
- `parse(args: &Value) -> Result<AskInvocation, ParseError>` — total
  validation from arbitrary MCP `arguments` JSON to a typed
  invocation, with typed `ParseError` naming the offending JSON path
  (e.g. `options.limit`).
- `AskInvocation` — all SQL clauses surfaced as `Option<…>`: `strict`,
  `using`, `model`, `limit`, `min_score`, `depth`, `temperature`,
  `seed`, `cache_ttl`, `nocache`. `None` means "no override" — the
  engine applies defaults downstream, not this module.
- `TOOL_NAME = "reddb.ask"`, `SCHEMA_DRAFT`, plus pinned
  range/default constants (`LIMIT_MIN/MAX/DEFAULT`, `DEPTH_*`,
  `MIN_SCORE_*`, `TEMPERATURE_*`).

Policy pinned by tests:
- Schema rejects unknown keys at both `arguments` and `options.*`
  levels (`additionalProperties: false`); typos like `tempurature`
  fail loud rather than being silently dropped.
- The schema's options keys (`limit/min_score/depth/.../cache/...`)
  match the parser's accepted set 1:1 — schema-vs-parser drift would
  be caught by `input_schema_options_keys_match_parser`.
- `cache` and `nocache: true` are mutually exclusive (matches the
  SQL parser); `nocache: false` paired with `cache` is benign.
- Ranges enforced: limit 1..=200, depth 0..=10, min_score 0..=1,
  temperature 0..=2 — mirrors the values pinned in the schema's
  `minimum`/`maximum`, so a wiring slice editing one without the
  other will fail the range tests.
- `Some(0)` for seed and `Some(0.0)` for temperature preserved (no
  `unwrap_or(0)` regressions — the same guard #400/#403 already
  pin).
- Question is preserved untrimmed; trim is a non-empty check only.
- `seed` rejects non-integer numbers (`1.5`) and negative values
  with distinct errors (`WrongType` vs `OutOfRange`).
- Descriptor is byte-deterministic across calls
  (`descriptor_is_deterministic`).

Description text pinned to mention "citation", "sources_flat", and
"URN" so the user-visible promise stays loud
(`description_emphasises_grounding`).

Deferred to follow-up slices (each independently shippable):

- Wire `descriptor()` into the MCP server's tool registry — likely
  alongside the existing tool-list response handler.
- Dispatch path: on `tools/call` for `reddb.ask`, run `parse()`,
  on `Ok` build an `AskQuery` and run `execute_ask`, on `Err` map
  the `ParseError` variant to the MCP error frame (the typed path
  is already machine-readable).
- Map `AskInvocation` → the existing `AskQuery` struct (parity with
  the SQL parser's output — already covers most fields per #400's
  slice 2).
- Optional MCP progressive-response streaming (mirror #405's SSE
  framing).
- Integration test from an MCP client harness exercising
  `tools/list` + `tools/call`.
- Docs in `docs/api/mcp.md` with worked examples for `STRICT`,
  `CACHE TTL`, and a citation walk-through.

Deep module is the load-bearing piece; remaining slices are
transport-layer wiring and can land independently. Issue stays open
with this progress note.

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib runtime::ai::mcp_ask_tool`
  → 34 passed.
