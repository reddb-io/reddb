# Docs sweep: parameterized form as default in vectors/query/driver guides [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/374

Labels: needs-triage

GitHub issue number: #374

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Sweep the docs so the parameterized form is the default shown to new users. Replace string-concat / `JSON.stringify` examples with `db.query(sql, params)`.

Pages in scope:

- `docs/data-models/vectors.md` — every SEARCH SIMILAR / INSERT example
- `docs/vectors/hnsw.md`, `docs/vectors/ivf.md`
- `docs/query/select.md`, `docs/query/insert.md`, `docs/query/update.md`, `docs/query/delete.md`, `docs/query/search-commands.md`, `docs/query/universal.md`
- `docs/guides/javascript-typescript-driver.md` and equivalent driver guides
- `docs/api/embedded.md`, `docs/api/http.md`, `docs/api/postgres-wire.md`
- `docs/getting-started/quick-start.md`

Add a "Safe parameter binding" section to the JS/TS driver guide showing the vector example.

## Acceptance criteria

- [ ] All listed docs updated.
- [ ] No vector or user-input example in docs uses string concatenation any longer.
- [ ] Each driver guide shows the parameterized form as the first example.
- [ ] Cross-link to the ADR (#352) from the relevant driver and query pages.

## Blocked by

- #361
- #362
- #363

## Progress

Slice 1 (commit f74c55c0): `docs/clients/drivers/go.md` got the
"Safe parameter binding" section + native-type table + FEATURE_PARAMS
gating note. Brought the Go hub page in sync with the driver README.

Slice 3 (this commit): `docs/clients/drivers/python.md` gets the same
treatment — new "Safe parameter binding" section between API surface
and Errors, native-type table mirroring `py_to_param_value` in
`drivers/python/src/high_level.rs`, gRPC backend `PARAMS_UNSUPPORTED`
gotcha called out, and the error code added to the stable codes table.
Variadic and `params=` kwarg forms both shown — `db.query` accepts
both per `drivers/python/tests/test_params.py`. Mixed-list /
bool-before-int gotchas surfaced. No ADR cross-link yet (PRD #352 ADR
still hasn't landed).

Slice 2 (commit 2e3233a6): `docs/guides/javascript-typescript-driver.md`
got the same treatment.

- New "4. Safe parameter binding" section sits between "Query and
  mutate data" and "Error handling", mirroring the Go hub page
  ordering so the params surface is visible before config noise.
- Scalar (int/text/null) + vector (Float32Array → HNSW SEARCH SIMILAR)
  examples — the vector example is what the issue explicitly
  requested for the JS/TS guide.
- Native JS → engine type mapping table inlined; covers `null` /
  `undefined`, `bigint`, integer-vs-float `number`, `Uint8Array` /
  `Buffer`, `Float32Array` / `Float64Array` / `number[]`, the
  `$bytes` / `$ts` / `$uuid` envelopes, and the plain-object → Json
  fallback. Matches `encodeValue` in `drivers/js/src/redwire.js`.
- Empty-params byte-equality call-out so operators inspecting the
  wire know upgrading the SDK is a no-op for un-parameterized
  workloads.
- `PARAMS_UNSUPPORTED` error code mentioned so callers know how the
  old-server failure surfaces.
- "Available methods" list now points `db.query(sql)` at the new
  section.
- Subsequent section numbers (5/6/7) bumped accordingly.

No ADR cross-link yet — the parameterized-queries ADR for PRD #352
still hasn't landed.

Deferred to follow-up slices (each independently shippable):

- `docs/clients/drivers/python.md`, `python-asyncio.md`, `rust.md`,
  `bun.md`, `dart.md`, `php.md`, `cpp.md`, `zig.md` hub pages — same
  treatment.
- `docs/data-models/vectors.md` (every SEARCH SIMILAR / INSERT
  example), `docs/vectors/hnsw.md`, `docs/vectors/ivf.md`.
- `docs/query/select.md`, `insert.md`, `update.md`, `delete.md`,
  `search-commands.md`, `universal.md` — replace string-concat
  examples with `db.query(sql, params)`.
- `docs/api/embedded.md`, `docs/api/http.md`, `docs/api/postgres-wire.md`.
- `docs/getting-started/quick-start.md`.

Verification (this slice):
- No code touched, no behavior change.
- `cargo check` / `pnpm test` not relevant — pure docs change.

