## Parent

Parent: #53

## What to build

Extract a deep RedWire MessageKind Catalog Module so the wire-spec invariants for each kind live in one inspectable Interface instead of being implicit in `MessageKind::from_u8` plus comments grouping numeric ranges. Today the data-plane / handshake / control / streamed-response classification is encoded only in source-comment groupings, and properties such as "may carry `MORE_FRAMES`", "is server→client only", "carries a row payload", and "is part of the handshake gate" are recomputed in dispatch and tests.

The completed slice should preserve current behavior (every kind keeps its byte value, every accept/reject decision in the dispatch loop is unchanged) while exposing those properties as catalog facts.

## Acceptance criteria

- [ ] Every shipped `MessageKind` byte value remains stable (catalog drift would be a wire-break).
- [ ] The catalog Interface exposes: kind class (data plane / handshake / control / streamed), allowed flag bits, and direction (client→server, server→client, either).
- [ ] Dispatch loop accept/reject decisions for handshake-gated kinds (e.g. only `Hello`/`Ping` before auth) consult the catalog rather than hard-coded match arms.
- [ ] Streamed-response kinds (BulkStreamRows, RowDescription, StreamEnd) declare their `MORE_FRAMES` invariant through the catalog; encoder and decoder cross-check it.
- [ ] Focused tests cover: unknown kind rejected, every catalog entry has a unique byte, every entry round-trips through `from_u8`.
- [ ] `cargo check` passes.

## Blocked by

- draft-53-redwire-frame-builder
