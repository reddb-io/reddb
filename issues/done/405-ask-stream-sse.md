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

Slice 2: parser flag + buffered HTTP SSE snapshot landed.

What changed:

- `ASK '...' STREAM` now parses into `AskQuery { stream: true }`.
  Existing non-streaming ASK queries keep `stream: false`, and gRPC's
  manual `AskQuery` construction is pinned to `stream: false`.
- `/query` detects `AskQuery.stream` and returns `Content-Type:
  text/event-stream` with frames produced by `SseFrameEncoder`.
- The current HTTP slice is deliberately buffered: it reuses the
  existing non-streaming `execute_ask` result, then emits
  `sources -> answer_token -> validation`. That proves the parser,
  runtime result extraction, SSE encoding, and HTTP response contract
  end-to-end without introducing a new provider streaming seam.
- Pre-answer cost guard failures on `ASK ... STREAM` return an SSE
  `error` frame with the mapped HTTP-style status code in the frame
  payload instead of a JSON error body.
- While touching the ASK parser, `STRICT ON` was fixed to accept `ON`
  as the keyword token the lexer already emits.

Still deferred:

- True incremental provider token forwarding. The current
  `answer_token` frame contains the completed answer as one chunk
  because `execute_ask` still calls the non-streaming LLM transport.
- Mid-stream cost checkpoints after partial token emission.
- Writing the durable audit row before the terminal `validation`
  frame. The current terminal frame includes the client-visible audit
  summary from the ASK result fields only.
- A socket-level SSE integration test with a stub provider that emits
  tokens incrementally.

Verification (slice 2):

- `cargo test -p reddb-io-server --lib test_parse_dml_extended_literals_auto_embed_and_ask_forms`
  → 1 passed.
- `cargo test -p reddb-io-server --lib http_query_ask_stream`
  → 2 passed.
- `cargo check -p reddb-io-server` clean.
- `pnpm test` exited 0 but skipped because `target/debug/red` was not
  present.
- `pnpm typecheck` exited 1 while printing `TypeScript: No errors
  found`, matching the prior wrapper behavior.

Slice 3: OpenAI-compatible streamed deltas now fan out to multiple SSE `answer_token` frames.

What changed:

- `OpenAiPromptRequest` gained a `stream` flag.
- ASK STREAM calls OpenAI-compatible providers with `stream: true` and `stream_options.include_usage: true`.
- OpenAI-compatible SSE responses are parsed from `data:` frames, preserving each `delta.content` as an answer chunk while still accumulating the full answer for validation/audit/result parity.
- The runtime ASK result now carries `answer_tokens` when provider deltas are available.
- HTTP SSE serialization emits one `answer_token` frame per preserved delta, falling back to the full answer for non-streaming providers.
- Tests now use a streaming OpenAI-compatible stub and assert two ordered `answer_token` frames plus exactly one terminal `validation` frame.

Still deferred:

- The server transport still returns a buffered `HttpResponse` with `Content-Length`; true socket-level incremental writes and per-frame flushes still need a route/connection streaming response type.
- Mid-stream cost checkpoints still happen after provider completion in this slice because `AiTransport::request` still reads the provider body fully before parsing.
- A socket-level SSE integration test with delayed provider chunks remains needed to prove client-visible incremental delivery timing.

Verification (slice 3):

- `cargo check -q` passed.
- `cargo test -q -p reddb-io-server --lib openai_prompt_payload -- --test-threads=1` → 3 passed.
- `cargo test -q -p reddb-io-server --lib http_query_ask_stream -- --test-threads=1` → 2 passed.

Slice 4: `/query` now uses socket-level SSE writes for `ASK ... STREAM`.

What changed:

- TCP and TLS connection handling now checks for `POST /query` with an
  `ASK ... STREAM` body before falling back to the existing buffered
  `route()` path.
- The streaming path preserves the same surface, auth, and quota gates
  as `route()`.
- SSE responses now write HTTP headers without `Content-Length`, include
  `Cache-Control: no-cache`, and flush after each encoded SSE frame.
- Existing `route()` tests and non-streaming `/query` responses keep the
  regular `HttpResponse` path.
- Added a socket-level integration test that posts `ASK ... STREAM`,
  consumes the raw TCP response, and asserts SSE headers, no
  `Content-Length`, ordered token frames, and one terminal validation
  frame.

Still deferred:

- Provider-body parsing is still not live: `AiTransport::request` reads
  the provider response body fully before the SSE frames are written to
  the client socket.
- Mid-stream cost checkpoints after partial token emission still need the
  provider streaming receiver.

Verification (slice 4):

- `cargo check -q` passed.
- `cargo test -q -p reddb-io-server --lib http_query_ask_stream -- --test-threads=1`
  → 3 passed.

Slice 5: provider-body streaming receiver + mid-stream guard landed.

What changed:

- Socket-level `POST /query` for `ASK ... STREAM` now executes through a
  live SSE path instead of constructing the full SSE body after
  `execute_ask` returns.
- The streaming path writes HTTP SSE headers first, emits the `sources`
  frame after retrieval/pre-call guards, forwards each OpenAI-compatible
  `delta.content` as an `answer_token` frame while the provider body is
  still being read, and emits the terminal `validation` frame only after
  validation and audit recording complete.
- Added a blocking OpenAI-compatible streaming prompt reader for the live
  socket path. It parses provider `data:` lines incrementally and keeps
  the accumulated response/usage shape compatible with the existing
  non-streaming ASK result.
- The in-flight cost guard now checks emitted completion size and elapsed
  time before each forwarded token. If it trips after partial output, the
  client receives an SSE `error` frame and no terminal `validation` frame.
- Cached/non-live providers still fall back to result-derived
  `answer_token` frames, preserving the existing response shape.
- Added socket tests with a delayed provider stream to prove the first
  token reaches the client before the provider completes, plus a
  mid-stream cost-guard test that emits one token then terminates with an
  error frame.

Verification (slice 5):

- `cargo check -q -p reddb-io-server` passed.
- `cargo test -q -p reddb-io-server --lib http_query_ask_stream -- --test-threads=1`
  → 5 passed.
- `cargo test -q -p reddb-io-server --lib openai_streaming_prompt_response_collects_delta_chunks -- --test-threads=1`
  → 1 passed (with pre-existing serde_json macro warnings).
- `git diff --check` passed.
