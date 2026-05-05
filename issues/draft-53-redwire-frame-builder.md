## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep RedWire Frame Builder Module. Today `Frame::new` plus `with_stream` / `with_flags` are constructed ad hoc at every call site that emits a frame (handshake replies, query/result pairs, bulk-stream rows, error frames, notices). Compression policy, `MORE_FRAMES` sequencing, correlation-id propagation, and size-limit enforcement (`MAX_FRAME_SIZE`) live partly in callers and partly in `codec.rs`.

The completed slice should preserve current on-wire bytes while moving frame construction discipline behind a small Builder Interface used by the server-side dispatch loop. Wire format (header bytes, kind values, flag bits) does not change.

## Acceptance criteria

- [ ] All response-frame construction in the server dispatch loop goes through the Frame Builder Interface rather than calling `Frame::new` and `with_*` inline.
- [ ] Correlation-id propagation from request to response is owned by the builder, not duplicated at each call site.
- [ ] `MORE_FRAMES` sequencing for streamed responses (BulkStreamRows, RowDescription/StreamEnd) is expressed through the builder so the last-frame invariant is checked in one place.
- [ ] `MAX_FRAME_SIZE` enforcement (and the COMPRESSED-fallback path when zstd fails) is centralised in the builder, not re-checked per call site.
- [ ] On-wire bytes for representative frames are unchanged (round-trip tests pass; existing chaos / bulk tests pass).
- [ ] `cargo check` passes.

## Blocked by

None - can start immediately
