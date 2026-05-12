# ASK via embedded stdio JSON-RPC [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/406

Labels: enhancement

GitHub issue number: #406

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK with citations through the embedded stdio JSON-RPC transport.

The JSON-RPC `query` method already routes `ASK` SQL today — this slice ensures all the new fields (`citations`, `sources_flat`, `validation`, `cache_hit`, `cost_usd`) are present in the JSON-RPC response. Streaming via JSON-RPC notifications is opt-in.

## Acceptance criteria

- [ ] Embedded stdio JSON-RPC `query` method returns the full new ASK schema.
- [ ] JS SDK `db.query('ASK \'...\'')` round-trips all fields.
- [ ] `STREAM` over JSON-RPC works via notification frames (or documented as not-yet if streaming is HTTP-only).
- [ ] Integration test from JS SDK against embedded engine.

## Blocked by

- #393

## Progress

Slice 2: embedded stdio JSON-RPC non-streaming envelope adapter landed.

- `rpc_stdio::query_result_to_json` now detects `RuntimeQueryResult`
  rows from `statement == "ask"` and returns the canonical
  `AskResponseEnvelope` directly in the JSON-RPC `result` field
  instead of wrapping it as `{ statement, affected, columns, rows }`.
- The adapter parses runtime `Value::Json` blobs for `sources_flat`,
  `citations`, and `validation`, preserves citation/source URNs, and
  fills currently-unwired transport fields with conservative defaults:
  `cache_hit = false`, `cost_usd = 0`, `mode = "strict"`,
  `retry_count = 0`.
- JS SDK types now expose the ASK result shape for `query(\`ASK ...\`)`.
  The driver round-trip test pins that `db.query("ASK ...")` returns
  every field unchanged from the transport response.
- JS stdio README documents that `ASK ... STREAM` notifications are not
  wired in the JS stdio JSON-RPC client yet; HTTP streaming remains the
  incremental ASK path for now.

Remaining follow-up:

- A true JS SDK embedded-engine integration test still needs a
  stubbable ASK LLM/provider path. The current SQL ASK executor calls
  the real provider transport directly, so this slice uses a Rust
  stdio adapter test plus a JS SDK transport round-trip test instead of
  spawning embedded `red` for an actual ASK provider call.
- Runtime wiring for real cache hits, cost accounting, strict/lenient
  mode fallback, and retry count should populate the same envelope
  fields when those ASK slices land.
- JSON-RPC notification frames for `ASK ... STREAM` remain deferred.

Verification (this slice):
- `cargo test -p reddb-io-server --lib rpc_stdio::tests::ask_query_result_uses_canonical_envelope`
- `cargo check -p reddb-io-server`
- `node --test drivers/js/test/ask.test.mjs`
- `pnpm --dir drivers/js test` (JS smoke skipped: `target/debug/red`
  missing)
- `git diff --check`
- `pnpm test` (skipped: `target/debug/red` missing)
- `pnpm typecheck` (nonzero wrapper despite `TypeScript: No errors found`)

Slice 1: `AskResponseEnvelope` deep module landed at
`crates/reddb-server/src/runtime/ai/ask_response_envelope.rs` with 19
unit tests. Pure — no I/O, no transport, no clock. Pins the canonical
non-streaming ASK JSON shape that the embedded stdio JSON-RPC `query`
method returns, and that gRPC (#407), PG-wire (#408), and MCP non-
stream (#409) embed verbatim. Mirrors the slice-1 pattern of #395,
#396, #398, #400, #401, #402, #403, #405, #409, #411.

Exposes:

- `SourceRow { urn, payload }`, `Citation { marker, urn }`,
  `ValidationWarning { kind, detail }`, `ValidationError { kind, detail }`,
  `Validation { ok, warnings, errors }`.
- `Mode::{Strict, Lenient}` — *effective* mode after #396 fallback.
- `AskResult { answer, sources_flat, citations, validation, cache_hit,
  provider, model, prompt_tokens, completion_tokens, cost_usd,
  effective_mode, retry_count }`.
- `build(&AskResult) -> Value` — BTreeMap-backed JSON, keys
  alphabetised.

Output shape pinned by tests:

- top-level keys: `answer, cache_hit, citations, completion_tokens,
  cost_usd, mode, model, prompt_tokens, provider, retry_count,
  sources_flat, validation` (one test asserts the exact key set so a
  future field can't silently rename one);
- `citations` sorted by `marker` ascending; tie on marker is
  stable-sorted (pinned because `sort_by_key` is stable; an unstable
  sort would non-determinise the response and break #400);
- `sources_flat` preserves caller-provided order verbatim — post-RRF
  rank order is the contract since `[^N]` indexes into the array;
- `validation = {errors, ok, warnings}` matches the shape audit row
  (#402) and SSE terminal frame (#405) already pin, so HTTP clients
  share parsing code across the streaming and non-streaming paths;
- `mode` serialises as `"strict"` / `"lenient"` — the *effective*
  mode after provider-capability fallback (#396), mirrors the audit
  row #402 so what the caller sees matches what was stored;
- `prompt_tokens` / `completion_tokens` / `cost_usd` are flat at the
  top level (no `usage` nesting) — matches the audit row and SSE
  audit frame so transports don't need a re-shape step;
- empty `sources_flat` / `citations` serialise as `[]` not `null`
  (a `STRICT OFF` refusal can legitimately produce no citations; a
  missing key would break downstream `.length` access);
- `seed` and `temperature` are NOT in the response — recorded in the
  audit row only (leaking the seed would let a hostile caller replay
  deterministic answers, breaking the determinism privacy boundary);
- byte-stable across calls and across `clone()` inputs.

Deferred to follow-up slices (each independently shippable):

- Adapter in `execute_ask` that produces `AskResult` from the
  internal answer + retrieval state, then calls `build()` and stamps
  the bytes into the JSON-RPC `result` field. Hook lives in
  `rpc_stdio.rs` next to the existing `query` method dispatch.
- gRPC (#407) and PG-wire (#408) embed the same envelope — gRPC as
  a proto `google.protobuf.Struct` (or per-field mirror), PG-wire as
  jsonb columns of a single-row result.
- JS SDK round-trip test against embedded engine — depends on the
  adapter slice above plus the stubbable LLM transport refactor
  already deferred by #395/#396.
- JSON-RPC notification framing for `STREAM` — separate slice
  (likely a thin wrapper around the SSE frame encoder #405 so the
  on-wire JSON payloads match across transports).

Deep module is the load-bearing piece; remaining slices are wiring
and can land independently. Issue stays open with this progress note.

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib runtime::ai::ask_response_envelope`
  → 19 passed.
