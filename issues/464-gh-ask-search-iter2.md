---
status: open
tag: AFK
gh: 464
---

# [AFK] gh-464 iter 2: Audit + close remaining ASK/SEARCH conformance gaps

GitHub: reddb-io/reddb#464

## Iter 1 (already on main, commit 4771e27c)

- tests/e2e_ask_search_conformance.rs (507 lines, 4 passing tests):
  - SEARCH CONTEXT bucket coverage per model
  - empty-overlap ground-or-silent
  - ASK mock-provider context + sources
  - mock answer surfaces verbatim
  - deterministic aggregates with AI disabled
  - literal-bearing ASK routes through Stage 4 filter_values
- docs/guides/ask-your-database.md: retrieval-grounding vs deterministic-analytics boundary documented.

## Iter 2 — audit + close gaps

Read iter 1 test file. Walk the 6 acceptance bullets vs what the file actually pins. For any bullet NOT covered, add a targeted test (still mock-provider only). If all 6 covered, close as done.

Specifically check:
- [ ] SEARCH CONTEXT returns relevant **vectors** when present (iter 1 covered rows/documents/KV/graph but vectors may be untested)
- [ ] Missing/unsupported analytics produce clear limitations or grounded responses (e.g. invalid SQL inside ASK, unsupported function)
- [ ] Docs describe boundary clearly (verify against current text)

## Notes
- Commit `Closes #464` if all gaps closed, else `Refs #464`.
- `CARGO_TARGET_DIR=.target-gh464-iter2`.
- Be surgical — only add tests for genuinely missing acceptance items.

## 2026-05-16 — iter 2 audit + 2 new tests (NOT YET COMMITTED — bash/git denied this session)

Audit findings vs iter 1 file (`tests/e2e_ask_search_conformance.rs`):

| Bullet | Status | Evidence |
|---|---|---|
| 1 SEARCH CONTEXT — rows/docs/KV/graph/**vectors** | ✓ covered | `search_context_returns_each_model_bucket` already asserts `!result.vectors.is_empty()` (line 164). Vector seeding present (lines 104-113). |
| 2 ASK mock provider receives + cites context | ✓ covered | `ask_with_mock_provider_cites_grounded_sources` |
| 3 Deterministic via SQL/engine | ✓ covered | `deterministic_sql_aggregate_skips_ai_provider` |
| 4 Missing/unsupported analytics → limitation | **partial** → now closed | iter 1 only covered missing-overlap on SEARCH. Iter 2 adds two tests: |
| 5 Tests use mock providers only | ✓ covered | All ASK paths point at `MockOpenAiStub`. |
| 6 Docs describe boundary | ✓ covered | `docs/guides/ask-your-database.md` § "Retrieval grounding vs deterministic analytics" table includes a row for "Missing or unsupported analytics inside an ASK question". |

### Iter 2 additions

- `ask_without_grounding_yields_clear_limitation` — ASK with a
  punctuation-only question (no usable tokens) must error with a
  message mentioning token/ground/usable AND the mock provider's
  request counter must stay at 0. Pins the "short-circuits with a
  structured error" half of the docs claim and asserts the provider
  boundary is honored even when the funnel empties out.

- `unsupported_sql_function_errors_deterministically` —
  `SELECT BOGUS_NONEXISTENT_FN(id) FROM incidents` must surface the
  evaluator's `EvalError::UnknownFunction` (matched via lowercase
  substring on "function" / "unknown" / "unsupported"). Pins the
  "engine never silently hands calculations off to the LLM" claim.

Module doc-comment updated to reflect 8 acceptance rows.

### Blocker

This sandbox session denied every `Bash` invocation (including
`git status`, `cargo test`, `git add`). The two new tests are written
but NOT compiled, NOT run, NOT committed. Next iter must:

1. `CARGO_TARGET_DIR=.target-gh464-iter2 cargo test --test e2e_ask_search_conformance`
   — verify both new tests compile and pass. If the ASK error message
   doesn't bubble through `execute_query` verbatim, relax/retune the
   substring assertion accordingly.
2. `git add tests/e2e_ask_search_conformance.rs issues/464-gh-ask-search-iter2.md`
3. Commit with `Closes #464` once green.
4. Move this issue to `issues/done/`.

If `ask_without_grounding_yields_clear_limitation` fails because the
ASK SQL path swallows the error and returns a "I don't know"-shaped
row instead, swap the `expect_err` assertion for a structured row
check: `sources_count == 0` AND `answer` contains "no" / "cannot" /
"insufficient" / etc., then assert `stub.request_count() == 0` (or
`>= 1` with answer pinned, depending on which branch the runtime
takes). The contract is "clear limitation OR grounded refusal" —
both are acceptable per the docs table.
