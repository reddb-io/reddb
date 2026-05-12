# ASK via Postgres-wire (non-stream) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/408

Labels: enhancement

GitHub issue number: #408

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK through Postgres-wire so existing psycopg / pgx / JDBC apps can call `ASK` without rewriting.

ASK over PG-wire is non-streaming (PG protocol does not naturally support SSE-style streaming for query results). Response rows expose the ASK fields as columns of a single-row result set: `answer text, sources_flat jsonb, citations jsonb, validation jsonb, provider text, model text, cost_usd numeric`, etc.

Depends on #360 (PG-wire extended protocol) for parameterized question text.

## Acceptance criteria

- [ ] `ASK '...'` executable via psycopg returns a single-row result with the documented columns.
- [ ] Same via pgx (Go).
- [ ] Same via JDBC (Java).
- [ ] No fallback to simple-query mode required.
- [ ] Integration tests against all three clients.
- [ ] Documented in `docs/api/postgres-wire.md` with examples.

## Blocked by

- #393
- #360

## Progress

Slice 1 (deep module): `PgWireAskRowEncoder` landed at
`crates/reddb-server/src/runtime/ai/pg_wire_ask_row_encoder.rs`.
Pure encoder — no I/O, no transport, no clock. Takes an `AskResult`
(reused from #406's `ask_response_envelope`) and returns
`AskRow { columns: Vec<ColumnDesc>, cells: Vec<Option<Vec<u8>>> }`,
the single-row Postgres-wire result-set body psycopg / pgx / JDBC
will see. Twelve columns alphabetised (same order as #406's envelope
JSON keys so a future bridge stays index-aligned).

Pinned contract (24 unit tests):

- 12 columns, alphabetical name order matches the envelope.
- `columns()` callable before any query runs (PG `Describe` codepath).
- OIDs from `wire::postgres::types::PgOid`: text(25) / bool(16) /
  jsonb(3802) / int8(20) / numeric(1700). Matches `pg_type_d.h`.
- `answer` UTF-8 round-trip including multibyte glyphs.
- `cache_hit` → `"t"` / `"f"`. Pinned both directions.
- `citations` jsonb is marker-ascending; empty → `[]` not `null`.
- `completion_tokens` / `prompt_tokens` / `retry_count` decimal int8.
- `cost_usd` uses `f64::to_string` canonical form so PG `numeric`
  parses without precision loss (rules out `float8` rounding that
  would break JDBC `BigDecimal`).
- `mode` → `"strict"` / `"lenient"` only.
- `sources_flat` preserves post-RRF input order (`[^N]` → index
  invariant); empty → `[]`.
- `validation` jsonb keys `errors` / `ok` / `warnings` always present;
  inner objects carry `detail` + `kind` (alphabetised by BTreeMap).
- No cells are ever `None` — empty data serialises as `[]` / `""`
  rather than PG NULL. The wiring slice can rely on this invariant
  when streaming `DataRow` lengths.
- `encode_is_deterministic_across_calls` — re-running on byte-equal
  input yields byte-equal output (cache-hit path #403 + determinism
  #400).
- `cells_index_aligns_with_columns_index` — wiring slice can iterate
  in lock-step.
- JSONB cells are compact, not pretty (wire-size budget + audit-row
  equality with #402).

Deferred to follow-up slices (each independently shippable):

- Parse `ASK '...'` in the PG-wire query path. Today simple-query
  routes through the SQL parser; this should already accept `ASK`
  after #391 lands the statement node, but the PG executor needs to
  recognise the row shape and route to `encode()` rather than the
  generic table-row codec.
- Hook `encode()` output into `RowDescription` (`columns()`) +
  `DataRow` (`cells`) + `CommandComplete` ("SELECT 1") frames.
- Extended-query path: `Parse` / `Bind` / `Describe` / `Execute` —
  blocked by #360. Today the codebase only supports the simple-query
  protocol. `columns()` is callable without an `AskResult`, so when
  #360 lands the `Describe` reply is trivial.
- Integration tests against psycopg, pgx, JDBC — each driver lives
  under its own driver directory and the harness is per-driver; this
  slice cannot land them without the wiring slice above first.
- `docs/api/postgres-wire.md` examples — deferred to the docs sweep
  in #412 (which already lists this page in scope).

Issue stays open with this progress note. The deep module is the
load-bearing piece; the remaining slices are mechanical wiring and
can land independently.
