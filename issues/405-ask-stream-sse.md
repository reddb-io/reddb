# ASK STREAM SSE via HTTP [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/405

Labels: needs-triage

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
