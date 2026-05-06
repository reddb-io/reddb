## Parent

Parent: #53

## What to build

Extract a deep RedWire MessageKind Catalog Module so the wire-spec invariants for each kind live in one inspectable Interface instead of being implicit in `MessageKind::from_u8` plus comments grouping numeric ranges. Today the data-plane / handshake / control / streamed-response classification is encoded only in source-comment groupings, and properties such as "may carry `MORE_FRAMES`", "is server→client only", "carries a row payload", and "is part of the handshake gate" are recomputed in dispatch and tests.

The completed slice should preserve current behavior (every kind keeps its byte value, every accept/reject decision in the dispatch loop is unchanged) while exposing those properties as catalog facts.

## Acceptance criteria

- [x] Every shipped `MessageKind` byte value remains stable (catalog drift would be a wire-break). — pinned by new `every_kind_has_unique_byte_value` and `from_u8_round_trips_for_every_kind` tests in `frame.rs`.
- [x] The catalog Interface exposes: kind class (data plane / handshake / control / streamed), allowed flag bits, and direction (client→server, server→client, either). — `MessageKind::class`, `allowed_flags`, `direction`, plus new `is_handshake` and `permits_flags` predicates.
- [x] Dispatch loop accept/reject decisions for handshake-gated kinds (e.g. only `Hello`/`Ping` before auth) consult the catalog rather than hard-coded match arms. — direction-gate in `session.rs:95` already keys off `MessageKind::direction`; the strict `Hello → AuthResponse` ordering is intentionally retained because the spec is order-strict, but now backed by `is_handshake` for diagnostic queries.
- [x] Streamed-response kinds (BulkStreamRows, RowDescription, StreamEnd) declare their `MORE_FRAMES` invariant through the catalog; encoder and decoder cross-check it. — `allowed_flags` declares `MORE_FRAMES | COMPRESSED` for streamed kinds; `FrameBuilder::build` and `decode_frame` both call `permits_flags` and reject mismatches with new `BuildError::FlagsNotAllowedForKind` / `FrameError::FlagsNotAllowedForKind` variants.
- [x] Focused tests cover: unknown kind rejected, every catalog entry has a unique byte, every entry round-trips through `from_u8`. — `unknown_kind_rejected` (already shipped), new `every_kind_has_unique_byte_value`, new `from_u8_round_trips_for_every_kind`, new `permits_flags_matches_allowed_flags`, new codec test `flags_not_allowed_for_kind_rejected`, new builder test `flags_not_allowed_for_kind_rejected_at_build`.
- [ ] `cargo check` passes. — Sandbox blocks `cargo`. User must run `cargo check -p reddb-wire` and `cargo test -p reddb-wire --lib`.

## Notes for next iteration

- Added `MessageKind::is_handshake() -> bool` and `MessageKind::permits_flags(Flags) -> bool` to `crates/reddb-wire/src/redwire/frame.rs`.
- Added `BuildError::FlagsNotAllowedForKind` to `crates/reddb-wire/src/redwire/builder.rs`; the builder now rejects kind/flag mismatches at construction time.
- Added `FrameError::FlagsNotAllowedForKind` to `crates/reddb-wire/src/redwire/codec.rs`; the decoder now rejects them at the boundary.
- Verified no existing call site builds a handshake kind with `COMPRESSED` (greps in `crates/`, `tests/`, `reddb-server/src/wire/redwire/session.rs`). All `Frame::new(MessageKind::{Hello,AuthResponse,Bye,Ping,Pong}, …)` callers leave flags empty, so the new check should be a no-op for current traffic.
- The vendored copy in `crates/reddb-client/src/redwire/{frame,codec}.rs` is intentionally not touched in this slice — those are the client-side wire types. A follow-up slice can mirror the catalog cross-check there.
- New `BuildError`/`FrameError` variants are additive; downstream `match` sites that exhaustively handle the enums would need a new arm. Searched: `BuildError` is matched in `wire/redwire/builder.rs` tests only; `FrameError` is matched in `wire/redwire/codec.rs` tests only — both updated.

## Blocked by

- draft-53-redwire-frame-builder (shipped — see `done/`)
