# Audit obligatory to red_ask_audit (AuditRecordBuilder) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/402

Labels: enhancement

GitHub issue number: #402

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Every ASK call writes a row to the `red_ask_audit` system collection (per ADR-319 `red_*` convention).

Schema:
`ts, tenant, user, role, question, sources_urns, provider, model, prompt_tokens, completion_tokens, cost_usd, answer_hash, citations, cache_hit, mode, validation_ok, retry_count, errors`.

Full answer text is **not** stored unless `ask.audit.include_answer = true`. Retention configurable via `ask.audit.retention_days` (default 90).

Introduces `AuditRecordBuilder` deep module — pure function from call state → row dict, with redaction policy applied.

## Acceptance criteria

- [ ] `AuditRecordBuilder` deep module: unit tests verifying every field, `answer_hash` is deterministic SHA-256, `include_answer` toggles content vs hash.
- [ ] `red_ask_audit` collection auto-created on first ASK.
- [ ] Audit row written before answer returns to client.
- [ ] Retention purge runs per `ask.audit.retention_days`.
- [ ] Integration test: 5 ASK calls produce 5 audit rows with correct fields.
- [ ] Integration test: `include_answer` flag toggle changes stored shape.

## Blocked by

- #393

## Progress

Slice 1: `AuditRecordBuilder` deep module landed at
`crates/reddb-server/src/runtime/ai/audit_record_builder.rs` with 15
unit tests. Pure — no I/O, no clock, no collection access. Exposes:

- `Settings { include_answer }` (default `false`).
- `CallState<'a> { ts_nanos, tenant, user, role, question,
  sources_urns, provider, model, prompt_tokens, completion_tokens,
  cost_usd, answer, citations, cache_hit, effective_mode,
  validation_ok, retry_count, errors }` — caller injects wall time so
  the builder stays deterministic.
- `build(state, settings) -> BTreeMap<&'static str, Value>` — produces
  one audit row. Keys pinned by `build_emits_every_required_field`.
- `answer_hash(answer) -> String` — lowercase hex SHA-256 of the
  answer text. Pinned against known SHA-256 of `""` and `"hello"`.

Policy pinned by tests:
- Every required column from the spec is emitted (`ts`, `tenant`,
  `user`, `role`, `question`, `sources_urns`, `provider`, `model`,
  `prompt_tokens`, `completion_tokens`, `cost_usd`, `answer_hash`,
  `citations`, `cache_hit`, `mode`, `validation_ok`, `retry_count`,
  `errors`).
- `answer_hash` is recorded regardless of `include_answer` so
  operators can deduplicate calls without retaining content.
- `include_answer = true` adds the `answer` key alongside
  `answer_hash` (additive — no other key shifts).
- `mode` serializes as `"strict"` / `"lenient"` based on the
  *effective* mode after provider-capability fallback (#396), so the
  audit row reflects what actually ran.
- `errors` is `[{kind, detail}, ...]`; `kind` is `"malformed"` or
  `"out_of_range"`, mapping `ValidationErrorKind` from #395.
- Empty identity fields are allowed (embedded usage with no auth
  context) and serialize as empty strings, not null.
- Source order is preserved verbatim from the input — the audit row
  reflects RRF ranking (#398) so post-hoc "what did the model see, in
  what order" stays honest.
- Cache hits still record `cost_usd`/`prompt_tokens` (as zero) so
  downstream `SUM()` aggregations never see a missing-field surprise.
- Re-running `build` on the same inputs produces a byte-equal map,
  pinned by `build_is_deterministic_across_calls` — required by the
  ASK determinism contract (#400).

Deferred to follow-up slices (each independently shippable):

- Auto-create the `red_ask_audit` collection on first ASK with the
  matching schema (`ts`, `tenant`, ..., `errors`). Needs a small
  bootstrap on the runtime side that's idempotent across replicas.
- Wire `build()` into `execute_ask` and write the row before the
  answer returns to the client. Audit-before-emit is the AC, so the
  insert path must be on the synchronous path.
- Surface `ask.audit.include_answer` and `ask.audit.retention_days`
  in runtime config (TOML/KV plumbing identical to #401 settings).
- Retention purge: nightly job that deletes rows older than
  `retention_days`. Default 90.
- Cluster forwarding (#410): per the existing replication topology,
  audit rows generated on a follower should ride the same path as
  other writes — track in #410.
- Integration tests:
  - 5 ASK calls → 5 audit rows with correct fields;
  - `include_answer` flag toggle changes stored shape.
  Both depend on the wiring slice above plus the stubbable LLM
  transport refactor already noted by #395/#396.

Deep module is the load-bearing piece; the remaining slices are
mechanical wiring and can land independently. Issue stays open with
this progress note (mirrors slice 1 pattern of #395, #396, #398,
#400, #401).
