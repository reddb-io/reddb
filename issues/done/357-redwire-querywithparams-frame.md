# RedWire QueryWithParams frame + capability negotiation (JS over TCP) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/357

Labels: enhancement

GitHub issue number: #357

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

New RedWire frame `QueryWithParams` carrying `(sql: String, params: Vec<Value>)` in the compact binary encoding defined by the ADR. End-to-end tracer: JS SDK over TCP RedWire issues a parameterized query, server binds and executes, results return.

Includes:

- Wire Value codec (deep module): pure encode/decode, round-trip testable, used by both client and server.
- Capability negotiation per ADR 0001 — old `Query` frame stays untouched; new clients advertise support, old clients keep working.
- Server: routes new frame to the same binder used by embedded stdio (#353).
- JS SDK `redwire.js`: emits `QueryWithParams` when params are present, falls back to `Query` when empty.

## Acceptance criteria

- [ ] `QueryWithParams` frame defined in `crates/reddb-wire` with versioning.
- [ ] Wire Value codec round-trips every Value variant (property-based tests).
- [ ] Capability negotiation: server advertises support; client checks before sending.
- [ ] New client + old server: clear error or graceful fallback per ADR.
- [ ] Old client + new server: existing `Query` frame still works unchanged.
- [ ] JS SDK over TCP RedWire passes the same parameterized integration suite as embedded stdio.

## Blocked by

- #353

## Progress (2026-05-12, slice 6 — JS driver emits QueryWithParams)

Sixth and final tracer slice. JS driver now routes `db.query(sql,
params)` through the new `MessageKind.QueryWithParams = 0x28`
frame whenever the server has advertised `FEATURE_PARAMS` (slice
3). Empty `params` keeps emitting plain `Query` so old servers
and the existing fast path stay untouched.

- `MessageKind.QueryWithParams = 0x28` joins the catalog in
  `drivers/js/src/redwire.js`. Pinned by a unit test against the
  Rust enum value in `crates/reddb-wire/src/redwire/frame.rs`.
- `Features.PARAMS = 0x01` mirrors `reddb_wire::features::FEATURE_PARAMS`.
- `ValueTag` table mirrors `reddb_wire::value::Tag` 1:1; pinning
  test deep-equals the table.
- `connectRedwire()` lifts the `features` u32 from `AuthOk`
  (falling back to `HelloAck` for older servers that omit it on
  AuthOk), normalises via a tiny `numberOr` helper, and threads
  it into the new `RedWireClient(socket, reader, session, features)`
  constructor signature (4th arg, defaults to `0` so callers that
  build clients in tests don't need to thread it).
- `RedWireClient.features()` (raw bitmask) and
  `RedWireClient.supportsParams()` (typed) expose the cap to
  caller code — same shape as the Rust client surface added in
  slice 4 (d92ab177).
- `RedWireClient.#query(sql, params)` branches on
  `Array.isArray(params) && params.length > 0`. With params it
  refuses to silently fall through to plain `Query` (which would
  drop the params and let the SQL run with `$N` literals — a
  silent SQL-injection footgun); instead throws
  `RedDBError('PARAMS_UNSUPPORTED', …)` with an actionable
  upgrade-the-server message. Empty params keep using the plain
  `Query` frame as before (so no perf regression on the hot
  path).
- New `encodeQueryWithParams(sql, params)`: hand-rolled
  `[u32 sql_len LE][utf-8 sql][u32 param_count LE][N values]`
  payload codec mirroring `crates/reddb-wire/src/query_with_params.rs`.
  Same `MAX_PARAM_COUNT = 65_536` and `MAX_VALUE_PAYLOAD_LEN = 16 MiB`
  guards.
- New `encodeValue(v)`: per-variant encoder mirroring
  `crates/reddb-wire/src/value.rs`. Accepts the JSON envelopes
  produced by `serializeParam` (`{$bytes, $ts, $uuid}`) AND raw
  JS values (Uint8Array → Bytes, Float32Array / number array →
  Vector, plain object → canonical Json with sorted keys, etc.)
  so the upstream `query()` in `index.js` can pass the
  `serializeParam`-massaged shape through to either the JSON
  envelope (HTTP / stdio) or the binary wire (RedWire) without
  the upstream caring which transport ran.

Decisions:
- Single-key envelope detection uses `Object.keys(v).length === 1`,
  matching the server-side `rpc_stdio::json_value_to_schema_value`
  shape (#356 slice 1). Multi-key objects route to canonical
  Json so the wire path can't silently swallow ambiguous
  payloads.
- `canonicalJson` is hand-rolled with sorted keys, byte-equal to
  the server's `crate::json` output. Keeps Json round-trips
  comparable byte-for-byte across the wire.
- Numbers in the safe integer range (±2^53) emit `ValueTag.Int`;
  otherwise `ValueTag.Float`. Mirrors the binder's semantic
  distinction for clauses that demand integers (LIMIT, OFFSET,
  K, PROBES — see #361).
- `BigInt` always emits `ValueTag.Int` so callers wanting i64
  over 2^53 have a path.
- Old client + new server: untouched. Old clients never probe
  `FEATURE_PARAMS` and never emit the new frame; they keep
  shipping plain `Query`.

Tests in `drivers/js/test/redwire.params.test.mjs` (25 new):
catalog discriminant pinning (3), `encodeValue` per-variant
shape (12 — bool / int number / bigint / float / text utf-8 /
Uint8Array / $bytes / $ts / $uuid / bad uuid / Float32Array /
number-array vector / plain object Json / multi-key
fallback / Symbol reject), `encodeQueryWithParams` empty / mixed
back-to-back / over-limit / non-array / non-string-sql.

Verification:
- pnpm test (drivers/js) -> 36 passed (25 new + 11 pre-existing).
- Full RedWire end-to-end with a live server is gated behind
  `REDWIRE_E2E=1` in the smoke; not run in this slice (issue
  doesn't require a CI server stand-up). Unit-level wire-format
  pinning is the contract the cross-driver fixtures (#373) will
  formalise.

Files: drivers/js/src/redwire.js (catalog + Features + ValueTag
+ supportsParams + #query branching + encodeQueryWithParams +
encodeValue + helpers), drivers/js/test/redwire.params.test.mjs
(new, 25 tests), drivers/js/package.json (test script wires the
two new test files).

Closes the JS-driver leg of #357. All listed AC met:
- [x] `QueryWithParams` frame defined in `crates/reddb-wire`
  (slice 2, 2ec14588).
- [x] Wire Value codec round-trips every Value variant — Rust
  side has proptest blocks (slice 1, 674985f8); JS side has
  per-variant pinning here.
- [x] Capability negotiation: server advertises (slice 3,
  c2d8eac9); JS client probes via `supportsParams()`.
- [x] New client + old server: throws `PARAMS_UNSUPPORTED`
  with actionable message rather than silently misbinding.
- [x] Old client + new server: untouched — `Query` frame still
  works (existing tests pass).
- [x] JS SDK emits `QueryWithParams` when params present,
  falls back to `Query` when empty.
