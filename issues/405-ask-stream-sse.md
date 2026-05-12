# ASK STREAM SSE via HTTP [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/405

Labels: enhancement

GitHub issue number: #405

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Adds `ASK '...' STREAM` Server-Sent Events transport over HTTP. SSE frame order:

1. `sources` frame with the full `sources_flat` and URNs.
2. `answer_token` frames with incremental answer text.
3. Terminal `validation` frame with `ok` + warnings + audit summary.

Validation runs server-side before the terminal frame is emitted. Cache, audit, and cost guards apply identically to the non-streaming path.

Streaming is HTTP-only in this slice (PG-wire is non-stream; embedded stdio and MCP support streaming via their own framing — separate slices).

## Acceptance criteria

- [ ] `ASK '...' STREAM` over HTTP returns SSE with three frame kinds.
- [ ] `sources` frame arrives first with complete `sources_flat`.
- [ ] `answer_token` frames stream tokens as the LLM produces them.
- [ ] Terminal `validation` frame is emitted exactly once.
- [ ] Audit row is written before terminal frame.
- [ ] Integration test consuming SSE with a stub provider that emits tokens.
- [ ] Cost guard mid-stream still triggers correctly (terminates SSE with error frame).

## Blocked by

- #395

## Progress

Slice 1: `SseFrameEncoder` deep module landed at
`crates/reddb-server/src/runtime/ai/sse_frame_encoder.rs` with 16
unit tests. Pure — no I/O, no transport, no LLM. Pins the on-wire
SSE bytes for every frame kind so the HTTP wiring slice can rely on
them.

Exposes:

- `Frame::{Sources, AnswerToken, Validation, Error}` — the four
  frame kinds the spec defines.
- `SourceRow { urn, payload }`, `ValidationWarning { kind, detail }`,
  `AuditSummary { provider, model, prompt_tokens, completion_tokens,
  cache_hit }` — payload shapes shared with the non-streaming HTTP
  response so clients can reuse parsers across transports.
- `event::{SOURCES, ANSWER_TOKEN, VALIDATION, ERROR}` — event-name
  constants pinned by tests.
- `encode(&Frame) -> String` — full SSE bytes including the `\n\n`
  terminator.

Wire format pinned by tests:

- `event: <name>\n` followed by one or more `data: <line>\n` then a
  blank line terminator (`\n\n`). Triple-newline is rejected (would
  emit a spurious empty frame).
- Multi-line payloads split across multiple `data:` lines. The split
  branch is hit by a hand-crafted payload test even though
  `to_string_compact` never emits a literal `\n` for our shapes —
  pinned so a future swap to pretty-printing can't silently break
  framing.
- `answer_token` carries `{"text":"..."}` with full JSON string
  escaping (quotes, backslashes, control bytes, unicode), so the
  client has one parse path across all frame kinds.
- `error` carries `{"code":<u16>,"message":"..."}` matching the HTTP
  status the non-streaming path would have used (413 for cost-guard,
  504 for timeout, 422 for validation, 500 for provider errors).
- `validation` carries `{audit, ok, warnings}` — keys alphabetical
  thanks to the in-house BTreeMap-backed JSON encoder.
- Empty `sources_flat: []` and empty `text: ""` frames are
  well-formed (the streaming receiver may forward odd chunks from a
  poorly-behaved provider).

Why this module exists separately from the HTTP handler: SSE
framing is the bug surface — forgotten terminators, swallowed
newlines, escaping mistakes — and is independent from the
hyper/axum streaming wiring. Pinning it here lets the wiring slice
focus on backpressure, the LLM token forwarder, and the mid-stream
cost-guard interruption (#401), with the byte format already
locked.

Deferred to follow-up slices (each independently shippable):

- Parse `ASK '...' STREAM` in the SQL parser and thread the flag
  through `AskQuery` into `execute_ask`.
- HTTP handler at `/v1/ask/stream` (or method on the existing
  `/v1/ask` switching on `Accept: text/event-stream`) emitting:
  sources frame → token loop → terminal frame, using `encode()`.
- Mid-stream `CostGuardEvaluator` checkpoint (#401) → terminal
  `Frame::Error { code: 413 | 504, ... }`.
- Audit row (#402) written before terminal frame so the audit /
  client-visible contract matches the non-streaming path.
- Integration test with a stub provider that yields tokens
  incrementally (depends on the stubbable LLM transport refactor
  already deferred by #395/#396).

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib runtime::ai::sse_frame_encoder`
  → 16 passed.

Deep module is the load-bearing piece; remaining slices are
transport-layer wiring and can land independently. Issue stays open
with this progress note (mirrors slice 1 pattern of #395, #396,
#398, #400, #401, #402, #403).
