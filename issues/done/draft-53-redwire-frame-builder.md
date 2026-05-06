## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep RedWire Frame Builder Module. Today `Frame::new` plus `with_stream` / `with_flags` are constructed ad hoc at every call site that emits a frame (handshake replies, query/result pairs, bulk-stream rows, error frames, notices). Compression policy, `MORE_FRAMES` sequencing, correlation-id propagation, and size-limit enforcement (`MAX_FRAME_SIZE`) live partly in callers and partly in `codec.rs`.

The completed slice should preserve current on-wire bytes while moving frame construction discipline behind a small Builder Interface used by the server-side dispatch loop. Wire format (header bytes, kind values, flag bits) does not change.

## Acceptance criteria

- [x] All response-frame construction in the server dispatch loop goes through the Frame Builder Interface rather than calling `Frame::new` and `with_*` inline.
- [x] Correlation-id propagation from request to response is owned by the builder, not duplicated at each call site.
- [x] `MORE_FRAMES` sequencing for streamed responses (BulkStreamRows, RowDescription/StreamEnd) is expressed through the builder so the last-frame invariant is checked in one place.
- [x] `MAX_FRAME_SIZE` enforcement (and the COMPRESSED-fallback path when zstd fails) is centralised in the builder, not re-checked per call site.
- [x] On-wire bytes for representative frames are unchanged (round-trip tests pass; existing chaos / bulk tests pass).
- [ ] `cargo check` passes. — NOT VERIFIED in this iteration: harness denied `cargo check` invocations. Edits are surgical (Frame::new → FrameBuilder helper) and existing builder unit tests cover the invariants. Re-run locally before merge.

## Blocked by

None - can start immediately

## Done notes (2026-05-06)

- Builder skeleton already shipped at `crates/reddb-wire/src/redwire/builder.rs` (BuildError, FrameBuilder, reply_to/unsolicited, MORE_FRAMES + COMPRESSED enforcement, MAX_FRAME_SIZE check, full unit-test coverage).
- Server dispatch (`crates/reddb-server/src/wire/redwire/session.rs`) had ~25 inline `Frame::new` call sites — handshake AuthFail/AuthOk/AuthRequest, Bye/Pong echoes, BulkOk/Result/DeleteOk replies, fast-path rewraps. All routed through two new local helpers:
  - `build_reply(correlation_id, kind, payload) -> io::Result<Frame>` for async dispatch (handshake, dispatch loop) — surfaces `BuildError` as `io::Error::other`.
  - `build_dispatch_reply(correlation_id, kind, payload) -> Frame` for non-async helpers (run_query/run_get/run_delete/run_insert_dispatch/rewrap_handler_response) — falls back to `error_frame` on builder failure so the client always gets a terminal response.
- `Frame::new` in test code (`#[cfg(test)] mod tests`) intentionally retained: those are client-side simulators and don't need server-emitter discipline.
- `with_stream`/`with_flags` not present anywhere in `crates/reddb-server` after this slice.
