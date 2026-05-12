# EXPLAIN ASK shows retrieval plan without LLM call [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/411

Labels: enhancement

GitHub issue number: #411

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

`EXPLAIN ASK '...'` returns the retrieval plan, source budget allocation, provider selection, and estimated cost — without calling the LLM.

Useful for debugging expensive queries before paying token cost, and for understanding which provider/model would be selected by the failover ladder.

Same options apply (`USING`, `LIMIT`, `MIN_SCORE`, `DEPTH`).

## Acceptance criteria

- [ ] `EXPLAIN ASK '...'` parses and dispatches.
- [ ] Output shows: per-bucket retrieval plan, RRF budget allocation, source URNs that would be selected, chosen provider/model, estimated prompt tokens.
- [ ] No LLM call is made.
- [ ] No audit row written for EXPLAIN.
- [ ] Integration test with stub retrievals.

## Blocked by

- #398

## Progress

Slice 1: `ExplainPlanBuilder` deep module landed at
`crates/reddb-server/src/runtime/ai/explain_plan_builder.rs` with 17
unit tests. Pure — no I/O, no clock, no LLM, no audit row. Mirrors
the slice-1 pattern of #395, #396, #398, #400, #401, #402, #403, #405.

Exposes:

- `Inputs { question, mode, retrieval, fusion_limit, fusion_k_constant,
  depth, sources, provider, determinism, estimated_cost }`.
- `BucketPlan { bucket, top_k, min_score }` — one per RRF input.
- `PlannedSource { urn, rrf_score }` — projected post-fusion row.
- `ProviderSelection { name, model, supports_citations, supports_seed }`
  — selected by the failover ladder.
- `Mode::{Strict, Lenient}` — *effective* mode after #396 fallback.
- `Determinism { temperature, seed }` — `None` => key omitted.
- `EstimatedCost { prompt_tokens, max_completion_tokens }`.
- `build(&Inputs) -> Value` — BTreeMap-backed JSON, alphabetised keys.

Output shape pinned by tests:
- top-level keys: `depth, determinism, estimated_cost, fusion, mode,
  provider, question, retrieval, sources` (one test asserts the exact
  ordered key set so a future field can't silently rename one);
- `mode` serialises as `"strict"` / `"lenient"`;
- `determinism.seed` and `determinism.temperature` are omitted when
  `None` (Anthropic-style no-seed, Local-class no-temperature), so the
  plan never claims a knob that won't reach the provider — same
  convention as the audit row #402;
- `Some(0)` seed preserved (guards against `unwrap_or(0)` regressions
  the way #400 and #403 already pin);
- `retrieval` preserves input bucket order; per-bucket `min_score`
  surfaced so BM25 0.4 vs cosine 0.7 stays visible to readers debugging
  `MIN_SCORE`;
- `sources` 1-indexed `rank`, input order preserved (caller hands rows
  in post-RRF rank order);
- empty `sources` and empty `retrieval` are well-formed (`[]`, not a
  missing key);
- `fusion.algorithm = "rrf"`, `k_constant = 60` — pins the Cormack
  2009 baseline from #398;
- byte-stable across calls with identical inputs.

Deferred to follow-up slices (each independently shippable):

- Parse `EXPLAIN ASK '...'` in the SQL parser; thread an `explain: bool`
  flag through `AskQuery` into `execute_ask`.
- `execute_ask` short-circuit: when `explain == true`, run retrieval +
  fusion + provider selection + determinism, assemble `Inputs`, return
  `build(&inputs).to_string_compact()`, and **skip** the LLM call and
  the audit-row write (AC: no LLM call, no audit row).
- Integration test with stub retrievals verifying URNs, RRF ranks,
  provider selection — depends on the stubbable retrieval / LLM
  transport refactor already deferred by #395/#396/#398.

Deep module is the load-bearing piece; remaining slices are mechanical
wiring and can land independently. Issue stays open with this progress
note.

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib runtime::ai::explain_plan_builder`
  → 17 passed.
