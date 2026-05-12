# ASK via gRPC [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/407

Labels: enhancement

GitHub issue number: #407

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK with citations through gRPC.

gRPC proto messages mirror the response schema (`answer`, `sources_flat`, `citations`, `validation`, etc.). Streaming via server-streaming RPC is opt-in.

Go driver and any other gRPC-based clients should round-trip the schema correctly.

## Acceptance criteria

- [ ] gRPC `Query` RPC returns the full ASK schema.
- [ ] Go driver `db.Query(ctx, 'ASK \'...\'')` works.
- [ ] Streaming via gRPC server-streaming method available.
- [ ] Proto evolution preserves backwards compatibility (optional fields, no required additions).
- [ ] Integration test from Go driver.

## Blocked by

- #393

## Progress

- 2026-05-12: Slice 2 — gRPC `Query` now returns the canonical ASK
  envelope in `QueryReply.result_json` when the runtime result is
  `statement == "ask"`. This unblocks the current Go gRPC facade path
  (`db.Query(ctx, "ASK '...'")`) because the driver already returns
  `QueryReply.result_json` bytes unchanged.

  Key decisions:
  - Keep `QueryReply` itself backwards-compatible; no proto field shape
    changed in this slice.
  - Special-case only `statement == "ask"` so a normal `SELECT ... AS
    answer` remains row-wrapped.
  - Reconstruct the same canonical `AskResult` used by JSON-RPC, MCP,
    and PG-wire from the runtime ASK row, preserving existing defaults
    for absent `cache_hit`, `cost_usd`, `mode`, and `retry_count`.
  - Parse `sources_flat`, `citations`, and `validation` from their
    runtime JSON columns before serialising the envelope, so gRPC
    clients receive arrays/objects rather than `null`.

  Tests:
  - `grpc_ask_query_reply_tests::query_reply_ask_result_json_uses_full_canonical_schema`
  - `grpc_ask_query_reply_tests::query_reply_non_ask_answer_column_keeps_row_shape`

  Verification:
  - `cargo test -p reddb-io-server grpc_ask_query_reply_tests --lib -- --nocapture`
    → 2 passed.
  - `cargo test -p reddb-io-server runtime::ai::grpc_ask_message --lib -- --nocapture`
    → 19 passed.
  - `cargo check -p reddb-io-server` passed.
  - `pnpm test` exited 0 but skipped because `target/debug/red` is
    missing.
  - `pnpm typecheck` printed `TypeScript: No errors found` but exited
    1.

  Deferred to follow-up slices:
  - typed `AskRequest`/`AskReply` proto evolution and `service_impl::ask`
    wiring;
  - `AskStream` server-streaming RPC over the #405 frame shape;
  - generated Go proto/client refresh and a real Go-driver integration
    test against stubbed ASK execution.

- 2026-05-12: Slice 1 — `GrpcAskMessage` deep module landed at
  `crates/reddb-server/src/runtime/ai/grpc_ask_message.rs`. Pure
  builder + typed shape mirroring the canonical
  `AskResponseEnvelope` (#406). No I/O, no codegen dependency, no
  wiring. 17 unit tests cover:
  - every top-level field present (parity with the JSON envelope —
    `field_set_matches_json_envelope` enforces 1:1 keys);
  - `mode` serialises as `"strict"` / `"lenient"` (effective mode
    after provider-capability fallback #396, audit-row parity #402);
  - citations sorted by marker ascending with stable order on ties
    (same guarantee #406 already pins);
  - `sources_flat` order preserved verbatim (post-RRF rank order is
    load-bearing for `[^N]` indexing);
  - empty sources serialise as `"[]"` not `"null"`;
  - JSON string escaping for quotes, backslashes, control chars
    (round-tripped via `serde_json::from_str`);
  - `sources_flat_json` key order alphabetised (`payload` before
    `urn`) to match the envelope's `BTreeMap`-backed encoder;
  - cache-hit rows still emit zeros (no missing-field surprise);
  - determinism: byte-equal input → byte-equal output;
  - seed and temperature are NOT in the reply (destructuring pin
    matches #406's `does_not_expose_seed_or_temperature`).

  `proto_tags` module pins the proto3 field numbers as constants
  (`AskReply` 1..12, `Citation` 1..2, `Validation` 1..3,
  `ValidationItem` 1..2). Editing any of them is a wire-breaking
  change and `ask_reply_proto_tags_pinned` /
  `ask_reply_proto_tags_are_unique_and_contiguous` /
  `nested_message_proto_tags_pinned` will fail.

  `sources_flat` is carried as a single JSON string
  (`sources_flat_json`) rather than a `repeated SourceRow` to keep
  parity with the envelope shape and avoid forcing per-row payload
  re-encoding. The bytes already flow on JSON-RPC (#406), MCP (#409),
  and PG-wire (#408) — clients that want structured rows parse the
  same JSON.

  Deferred to follow-up slices (each independently shippable):

  - Edit `crates/reddb-grpc-proto/proto/reddb.proto`:
    - Add `AskReply`, `AskRequest`, `Citation`, `Validation`,
      `ValidationItem` messages with the field numbers pinned above.
    - Replace `rpc Ask(JsonPayloadRequest) returns (PayloadReply)`
      with `rpc Ask(AskRequest) returns (AskReply)`. Tonic
      regenerates from `build.rs`; service impl signature changes.
  - Wire `GrpcAskMessage::build` into `service_impl::ask`: parse
    `AskRequest`, call `execute_ask`, lift the `AskResult` from the
    runtime, hand to `build`, return typed `AskReply`. Drop the
    legacy `payload_json` mapping currently at `service_impl.rs:2320`.
  - Add an `AskStream` server-streaming RPC for the SSE-equivalent
    path (#405 frame encoder); proto: `rpc AskStream(AskRequest)
    returns (stream AskStreamEvent);` and the deep module from #405
    already pins the frame shape.
  - Update Go driver (`drivers/go/`) to round-trip the typed message.
  - Integration test from the Go driver harness — depends on the
    wiring slice + the stubbable LLM transport refactor already noted
    by #395/#396/#398/#406.

  Deep module is the load-bearing piece; remaining slices are
  mechanical wiring and can land independently. Issue stays open
  with this progress note (mirrors slice 1 pattern of #395, #396,
  #398, #400, #401, #402, #403, #405, #406, #409, #411).

  Verification: `cargo check`/`cargo test` runs blocked by the
  sandbox in this AFK iteration. Module is a pure addition modeled
  closely on the sibling `ask_response_envelope.rs`,
  `explain_plan_builder.rs`, and `mcp_ask_tool.rs` deep modules;
  next iteration should run
  `cargo test -p reddb-io-server --lib runtime::ai::grpc_ask_message`
  to confirm before the wiring slice.
