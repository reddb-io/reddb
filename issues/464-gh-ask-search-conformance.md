---
status: open
tag: AFK
gh: 464
---

# [AFK] gh-464: Add ASK and SEARCH CONTEXT conformance for multi-model grounding

GitHub: reddb-io/reddb#464

## What to build

SEARCH CONTEXT returns relevant rows/documents/graph entities/vectors/KV when each exists. ASK with mock provider receives + cites context. Deterministic calculations stay on SQL/engine paths. Missing/unsupported analytics produce clear limitations or grounded responses. Tests use mock providers only.

## Acceptance criteria

- [ ] SEARCH CONTEXT returns relevant rows, documents, graph entities, vectors, and KV when each exists
- [ ] ASK with mock provider receives + cites context
- [ ] Deterministic calculations via SQL/engine, not invented by mock AI
- [ ] Missing/unsupported analytics produce clear limitations or grounded responses
- [ ] Tests use mock providers only (no external AI)
- [ ] Docs describe retrieval grounding vs deterministic analytics boundary

## Notes
- `CARGO_TARGET_DIR=.target-gh464`
- Commit `Closes #464` or `Refs` if partial

## 2026-05-16 — partial: conformance test + docs scaffold

Added (NOT YET COMMITTED — bash/cargo/git were denied for this iteration):

- `tests/e2e_ask_search_conformance.rs` — new integration test binary
  covering five acceptance rows:
    - `search_context_returns_each_model_bucket` exercises tables,
      documents, KV, graph nodes, and vectors when each backing
      collection exists ("gateway" query against a fresh multi-model
      fixture).
    - `search_context_no_match_yields_empty_grounded_response` proves
      the "ground or fall silent" contract — total entities = 0 when
      no collection has overlap.
    - `deterministic_sql_aggregate_skips_ai_provider` asserts
      `SELECT COUNT(*)` is exact and never reaches the AI provider
      (no env vars, no mock — would fail on any unintended provider
      call).
    - `ask_with_mock_provider_cites_grounded_sources` wires an inline
      OpenAI-compatible TCP stub via `REDDB_OPENAI_API_*` env vars,
      seeds an `INC-001` literal so Stage 4 `filter_values` grounds
      without an embedding API, runs `ASK 'show incidents matching
      INC-001 with status' STRICT OFF`, and asserts answer ==
      "the incident is mocked …", provider == "openai",
      sources_count > 0, sources_flat references the seeded
      collection, and the mock received at least one request.
  Mock-only — no external AI provider is contacted.

- `docs/guides/ask-your-database.md` — new "Retrieval grounding vs.
  deterministic analytics" section under "What's Happening Under the
  Hood" enumerating which surface (SQL planner, search index tiers,
  AskPipeline, LLM) owns which contract, and pointing at the
  conformance test as the CI gate.

Next iteration must:
1. Run `CARGO_TARGET_DIR=.target-gh464 cargo test --test
   e2e_ask_search_conformance` and address any compile/runtime fix-ups
   (KV tokenization, schema-vocabulary lookup, mock-port races).
2. `pnpm test` + `pnpm typecheck` if either are wired for Rust crates.
3. Commit with `Refs #464` (or `Closes #464` once CI is green) and
   move this issue file to `issues/done/`.

Risk notes:
- Env-var manipulation is process-global; the test takes
  `ASK_ENV_LOCK` but only inside this binary. Cargo runs each
  `tests/*.rs` as its own binary, so there's no cross-file leak.
- `WITH CONTEXT INDEX ON (id, title, status)` is needed so the
  field-tier index surfaces the row deterministically — without it
  the global-scan fallback still works but is order-sensitive.
- The mock stub answers both `/embeddings` and `/chat/completions`;
  the embedding response is well-formed JSON so Stage 3b
  `vector_search_scoped` doesn't choke even though we don't pin its
  output.
