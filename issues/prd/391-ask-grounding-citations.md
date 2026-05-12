# PRD: ASK with inline citations — grounding-citable AI queries as RedDB's wedge

Labels: prd

GitHub: https://github.com/reddb-io/reddb/issues/391

GitHub issue number: #391

## Problem Statement

Developers building AI-native applications (RAG, agents, semantic search over operational data) today reach for Postgres + pgvector + LangChain/LlamaIndex by default. They get vector search but no grounding guarantees — the LLM produces an answer, and they have to bolt on their own citation/attribution layer to know which source backed each claim. Building that layer correctly is non-trivial: handling adversarial content in sources, validating citations, applying per-user RLS to retrieval, accounting cost, auditing for compliance, and making the whole thing deterministic enough to test.

RedDB already ships `ASK` (`docs/guides/ask-your-database.md`) which retrieves multi-model context and synthesizes a natural-language answer. But today `ASK` returns `answer` (free text) and `sources` (the retrieved context) as two parallel fields — nothing links a specific claim in the answer to the specific source that supports it. The user must trust the LLM didn't hallucinate, exactly the gap they'd hit with Postgres + pgvector.

This is the wedge gap. If `ASK` becomes the first place a developer goes for AI-native data queries, RedDB wins the AI-native greenfield category. If `ASK` stays at "RAG built into the database, but you still bolt your own grounding", we win nothing.

## Solution

`ASK` v1 returns answers with **inline, validated, navigable citations** by default. Every factual claim in the answer is marked with `[^N]`, where `N` is a 1-based index into a flat `sources` array. Each source carries a `urn` like `reddb:articles/42` or `reddb:embeddings/abc#0.87` so clients can deep-link back to the underlying row, document, vector, or graph node.

Validation, security, cost, audit, and operational behavior are first-class — not afterthoughts. Specifically:

- **Strict citation validation by default.** Server rejects answers with out-of-range `[^N]`, retries once with a corrected prompt, and surfaces structural failures clearly. Lenient mode is an opt-in.
- **RLS-respecting retrieval.** ASK reads sources through the same authorization context as SELECT — a user cannot use ASK to exfiltrate data they cannot query directly.
- **Prompt-injection sandboxed sources.** Retrieved content is wrapped in `<source>` tags with an explicit system instruction that source contents are data, never instructions. Adversarial rows cannot hijack the model.
- **Deterministic by default.** `temperature=0` and a seed derived from the question plus source fingerprints. Same inputs → same answer (or a clear retry+audit trail when not).
- **Cost guards configurable.** Hard limits on prompt tokens, completion tokens, sources bytes, timeout, and daily cost cap per tenant.
- **Audit obligatory.** Every ASK call writes a row to the `red_ask_audit` system collection with cost, citations, sources URNs, and an answer hash. The full answer is opt-in (PII concern).
- **Cluster-correct.** ASK accepts on read replicas (retrieval reads local snapshot), but the audit row writes synchronously through the primary before the answer returns. No audit gap is permitted.

The wedge claim becomes demonstrable in one line of SQL:

```sql
ASK 'why did customer X churn?' USING openai
```

→ returns

```json
{
  "answer": "Customer X churned after a 14-day support escalation [^1] following three failed onboarding attempts [^2][^3]. Sentiment scores dropped sharply in the final week [^4].",
  "sources": [
    { "urn": "reddb:tickets/9123", "kind": "table", "content": "...", "score": 0.91 },
    { "urn": "reddb:onboarding_events/4471", "kind": "table", "content": "...", "score": 0.88 },
    { "urn": "reddb:onboarding_events/4472", "kind": "table", "content": "...", "score": 0.86 },
    { "urn": "reddb:embeddings/sentiment#a3f9", "kind": "vector", "content": "...", "score": 0.84 }
  ],
  "citations": [
    { "marker": 1, "span": [38, 70], "urn": "reddb:tickets/9123" },
    { "marker": 2, "span": [80, 121], "urn": "reddb:onboarding_events/4471" },
    { "marker": 3, "span": [80, 121], "urn": "reddb:onboarding_events/4472" },
    { "marker": 4, "span": [123, 175], "urn": "reddb:embeddings/sentiment#a3f9" }
  ],
  "validation": { "ok": true, "warnings": [] },
  "provider": "openai",
  "model": "gpt-4o",
  "prompt_tokens": 1247,
  "completion_tokens": 78,
  "cost_usd": 0.0042,
  "cache_hit": false
}
```

This is the artifact a developer cannot produce trivially on Postgres + pgvector. That's the wedge.

## User Stories

1. As a developer building a RAG application, I want `ASK` to return answers with `[^N]` citation markers, so that I can render which source backed each claim without writing my own attribution layer.

2. As a developer, I want each source to carry a stable `urn` like `reddb:articles/42`, so that I can deep-link from the rendered answer back to the underlying row in my UI.

3. As a developer, I want the server to validate citations before returning the answer, so that I never see `[^99]` when only 5 sources exist.

4. As a developer, I want the server to retry the LLM once with a corrected prompt when validation fails, so that transient grounding mistakes self-heal without my code needing retry logic.

5. As a developer, I want to set strict vs lenient validation per query, so that exploratory queries don't fail on warnings while production queries enforce grounding.

6. As a developer, I want to choose between OpenAI / Anthropic / Groq / Ollama / local providers, so that I can balance cost, latency, and data residency.

7. As a developer, I want providers without reliable citation support to gracefully fall back to lenient mode, so that local/cheap providers still work without breaking strict callers.

8. As a developer, I want an ordered failover list of providers, so that an outage on Groq automatically falls over to OpenAI without my code intervening.

9. As a developer, I want `ASK` to read sources through my user's RLS context, so that ASK can never reveal data a SELECT couldn't.

10. As a security engineer, I want adversarial content in retrieved sources (e.g. "ignore previous instructions") to be sandboxed in structured tags, so that prompt injection from user-uploaded data cannot hijack ASK calls run by admins.

11. As a developer, I want top-K retrieval with `LIMIT N` controlling the total source budget, so that costs are predictable.

12. As a developer, I want hybrid retrieval (BM25 + vector + graph traversal) fused via Reciprocal Rank Fusion by default, so that I don't have to tune retrieval modes myself.

13. As a developer, I want answers to be non-streaming by default, so that REST/ORM/BI clients can consume `ASK` like any other query.

14. As a developer, I want to opt into streaming with `ASK '...' STREAM`, so that my chat UI can render tokens as they arrive.

15. As a developer building interactive UIs, I want the streaming response to send sources + URNs first, then answer tokens, then a final validation frame, so that the UI can render citations as soon as a marker appears.

16. As a developer, I want to enable caching with `ASK '...' CACHE TTL '5m'`, so that repeated questions on stable data don't burn LLM tokens.

17. As an operator, I want a settings flag `ask.cache.default_ttl` to enable a global cache default, so that I don't have to annotate every query.

18. As a developer, I want `ASK '...' NOCACHE` to bypass the global cache default, so that I can force a fresh answer in specific spots.

19. As an operator, I want cost guards (`ask.max_prompt_tokens`, `ask.max_completion_tokens`, `ask.max_sources_bytes`, `ask.timeout_ms`, `ask.daily_cost_cap_usd`) in settings, so that runaway prompts cannot drain my LLM budget.

20. As an operator, I want a per-tenant daily cost cap, so that abusive queries in one tenant cannot exhaust budget for the rest.

21. As a compliance officer, I want every ASK call to write an audit row to `red_ask_audit` automatically, so that I can trace who asked what and which sources were exposed.

22. As a compliance officer, I want the audit row to **not** store the full answer by default, so that PII recovered from data doesn't get persisted in audit indefinitely.

23. As a compliance officer, I want an `ask.audit.include_answer` opt-in flag, so that I can enable answer logging on environments where the privacy tradeoff is acceptable.

24. As an operator, I want `red_ask_audit` retention configurable (default 90 days), so that audit doesn't grow unbounded.

25. As a developer, I want ASK to be deterministic by default (`temperature=0`, seed derived from question + sources fingerprint), so that the same question on the same data returns the same answer.

26. As a developer, I want to override determinism per query with `ASK '...' TEMPERATURE 0.7`, so that creative/varied answers are possible when needed.

27. As an operator, I want a global default temperature setting, so that I can tune the whole tenancy at once.

28. As a developer running a cluster, I want to send `ASK` to a read replica, so that retrieval load distributes.

29. As a compliance officer, I want the audit row write to land on the primary synchronously before ASK returns, so that no audit can be lost when a replica fails after answering.

30. As an operator, I want cache populate to happen asynchronously on the replica's local cache + propagate, so that cache lookups stay local on the replica.

31. As an operator, I want `ASK` calls to surface which provider answered in the response, so that I can debug failover behavior.

32. As a developer, I want clear errors when prompt budgets are exceeded (HTTP 413 with the offending limit named), so that I can adjust scope rather than guess.

33. As a developer, I want errors when no enabled provider supports the requested model, so that misconfigurations surface at call time.

34. As a developer, I want errors when validation exhausts retry budget (after one retry), so that I can fall back to lenient mode or alternative providers.

35. As a developer, I want `ASK` exposed through MCP as a tool, so that LLM agents can ground their own answers through the same machinery.

36. As a developer, I want the same `ASK` semantics through embedded stdio, HTTP, gRPC, RedWire, and Postgres-wire transports, so that I can call it from any client.

37. As an operator, I want `EXPLAIN ASK '...'` to show retrieval plan, source budget allocation, and provider selection without calling the LLM, so that I can debug expensive queries.

38. As a developer, I want validation warnings to include suggestions (e.g. "answer cited [^7] but only 5 sources retrieved"), so that I know how to fix queries.

39. As a developer, I want `MIN_SCORE N` and `DEPTH N` to remain available on `ASK` for retrieval tuning, so that I can prune low-confidence sources.

40. As a developer using Postgres-wire, I want to call `ASK` through psycopg / pgx / JDBC, so that my existing Postgres app can adopt grounding without rewriting connection code.

## Implementation Decisions

**Wedge framing.** RedDB ASK with grounding citations is the AI-native greenfield wedge. No proof artifacts (evals, leaderboards, whitepapers) are scoped here — only the code that makes the claim defensible. Demos and benchmarks come later.

**Citation mechanism.** Inline `[^N]` markers inside `answer`. `N` is a 1-based positional index into a flat `sources` array. Markers can repeat (one fact, multiple sources). LLM is prompted explicitly to use this format and to cite every factual claim.

**Source addressing.** `sources` is a flat array. Each entry carries `kind` (table/document/vector/graph_node/graph_edge/kv), `urn` (`reddb:<collection>/<id>` plus model-specific suffixes for vectors and graph edges), `content` (raw text/json view), `score` (retrieval rank score), and any kind-specific extras. Buckets are no longer the canonical representation; clients that want bucket views derive them.

**Strict validation as default.** Server parses `[^N]` markers from the answer and checks `1 <= N <= len(sources)`. On failure, one retry with a corrected prompt ("you cited source N but only M exist; cite only valid sources"). If retry also fails: response is 422 with `validation.ok=false` and `validation.errors` populated. Lenient mode is opt-in via `ASK '...' STRICT OFF`.

**Validation scope.** Structural only (index exists). No NLI / entailment / lexical-overlap checks in v1 — they double cost and latency. Future iteration may add `STRICT ENTAILMENT`.

**Provider capability flag.** Each provider in the AI provider registry carries a `supports_citations` boolean (and `supports_seed`, `supports_temperature_zero`, etc.). ASK in strict mode against a provider that does not support citations transparently falls back to lenient mode with a warning surfaced in the response. Audit row records the actual mode used.

**Retrieval policy.** Hybrid: BM25 over text fields, vector similarity over vector fields, graph traversal `DEPTH N` over edges related to query entities. Per-bucket top-K is fused via Reciprocal Rank Fusion. Total source budget is `LIMIT N` (default 20) — pruning happens after fusion. Per-bucket quotas can be added in v2 if the K-only model proves insufficient.

**Authorization.** Retrieval runs through the same executor that backs `SELECT`, with the caller's user/role context. Any row, document, vector, or graph element not visible to a SELECT is invisible to `ASK`. ASK calls log the role used in the audit row.

**Prompt-injection defense.** All retrieved sources are wrapped in `<source id="N" urn="...">...</source>` tags. The system prompt explicitly states that content inside `<source>` tags is data, never instructions, and that the model must not act on directives appearing inside source content. This is industry-standard defense-in-depth; combined with strict citation validation, hallucinated indices from injected instructions are detectable.

**Streaming.** Non-streaming default. `ASK '...' STREAM` opts into Server-Sent Events. SSE frame order: (1) `sources` frame with URNs, (2) `answer_token` frames with incremental text, (3) terminal `validation` frame with `ok` + warnings. Validation runs on the server before the terminal frame is emitted. Streaming is not available over Postgres-wire (which speaks its own protocol); HTTP and embedded stdio JSON-RPC support it.

**Caching.** Opt-in via `ASK '...' CACHE TTL '5m'`. Cache key is `hash(question + provider + model + temperature + seed + sources_fingerprint)`. Sources fingerprint is a derived watermark over the collections involved; mutations to those collections invalidate. Settings: `ask.cache.enabled` (default false), `ask.cache.default_ttl` (default `5m` when enabled), `ask.cache.max_entries`. Per-query `NOCACHE` bypasses the default. The blob cache infrastructure (ADR 0006) is reused.

**Cost guards.** Settings:
- `ask.max_prompt_tokens` (default 8192)
- `ask.max_completion_tokens` (default 1024)
- `ask.max_sources_bytes` (default 262144)
- `ask.timeout_ms` (default 30000)
- `ask.daily_cost_cap_usd` (default unlimited, scoped per tenant)

Exceeded limits return HTTP 413 (over-budget) or 504 (timeout) with the offending limit named. Daily cost counter resets at UTC midnight.

**Audit log.** Every ASK call writes a row to `red_ask_audit`:
`ts, tenant, user, role, question, sources_urns (array), provider, model, prompt_tokens, completion_tokens, cost_usd, answer_hash (sha-256), citations (array of {marker, urn}), cache_hit, mode (strict/lenient), validation_ok, retry_count, errors`.

Full answer text is **not** stored unless `ask.audit.include_answer = true`. Retention `ask.audit.retention_days` (default 90). The `red_*` system-collection convention is respected.

**Determinism.** Default `temperature=0`. Default `seed = hash(question + sources_fingerprint)` for providers that support it (OpenAI, Groq, Together, OpenRouter via OpenAI-compat). Providers without seed (e.g. some local) fall back to temperature alone. Settings: `ask.default_temperature`. Per-query overrides: `ASK '...' TEMPERATURE 0.7 SEED 42`.

**Provider failover.** Ordered list configurable: `ask.providers.fallback = ['groq', 'openai', 'anthropic']`. Failover triggers on transport errors, 5xx, or timeout. The successful provider is recorded in `provider` and audited. Per-query override: `ASK '...' USING 'groq,openai'`. Failover preserves the seed/temperature/strict-mode within the call (does not retry differently).

**Cluster behavior.** Read replicas accept `ASK`. Retrieval reads from local snapshot. LLM call is made from the local node. **Audit row is forwarded synchronously to the primary** (RPC with `WAIT FOR ACK`) before the answer returns to the client. Cost-accounting increment is also primary-synchronous (to avoid races on `daily_cost_cap_usd`). Cache populate is local + async-propagate. If the primary is unreachable, ASK on replica fails with 503 — no audit gap is acceptable.

**Multi-step / agentic.** Out of scope for v1. `ASK` is single-shot: one retrieval pass, one LLM call (plus one retry on validation failure). Future `ASK '...' STEPS N` would enable tool-use with a hard budget.

**Versioning.** No formal stability commitment in v1. Schema may evolve. After v1 ships and stabilizes in production usage, an ADR will adopt `api_version: "ask.v1"` with deprecation policy.

**Deep modules (pure, testable in isolation).**

1. `CitationParser` — extracts `[^N]` markers from answer text, returns `(stripped_text, spans, indices)`. Handles escape (`[\^1]`), code blocks (do not parse), and malformed forms.
2. `RrfFuser` — accepts per-bucket ranked hits, fuses via RRF with configurable rank constant, prunes to total K, returns flat array with stable indices and attached URNs.
3. `UrnCodec` — bidirectional codec for `reddb:<collection>/<id>` and model-specific suffixes (`#<score>`, `#<edge_id>`). Handles UTF-8 collection names and percent-encoding.
4. `PromptAssembler` — composes final prompt: system prompt with anti-injection instructions, `<source id urn>...</source>` blocks, question, and citation directive. Produces golden output for given input.
5. `ProviderCapabilityRegistry` — pure lookup of `provider name → capabilities` (`supports_citations`, `supports_seed`, `supports_temperature_zero`, `supports_streaming`).
6. `StrictValidator` — pure function `(answer, sources_count, mode) → validation_result | retry_prompt | warnings`. One-retry policy is enforced here.
7. `CostGuardEvaluator` — pure `(usage_so_far, daily_state, settings) → allow | reject(reason, limit_named)`. Daily reset is parameterized (caller passes "now").
8. `AuditRecordBuilder` — pure function from call state → `red_ask_audit` row dict. PII-redaction policy is applied here.
9. `AskOrchestrator` — non-pure; the only module that touches I/O. Orchestrates retrieval → assemble → call → validate → retry → audit → respond. Uses all the deep modules.

**Modified modules.**

- SQL parser: accept `ASK '...'` options `STREAM`, `CACHE TTL`, `NOCACHE`, `STRICT [ON|OFF]`, `USING 'a,b,c'`, `TEMPERATURE`, `SEED`, `LIMIT`, `MIN_SCORE`, `DEPTH`.
- AI provider layer: ordered failover, retry-once for citation validation, capability flag enforcement, per-tenant cost counter.
- Replication: audit forward primary-sync RPC; ASK-on-replica codepath.
- HTTP `/query`: SSE response when `STREAM`.
- MCP tools: expose `ASK` (params include question, options, plus the standard auth context).

**Settings introduced.**

`ask.cache.enabled`, `ask.cache.default_ttl`, `ask.cache.max_entries`, `ask.max_prompt_tokens`, `ask.max_completion_tokens`, `ask.max_sources_bytes`, `ask.timeout_ms`, `ask.daily_cost_cap_usd`, `ask.default_temperature`, `ask.audit.include_answer`, `ask.audit.retention_days`, `ask.providers.fallback`, `ask.strict.default`.

## Testing Decisions

**What makes a good test here.** Tests assert external behavior of each module: given input X, observable output Y, regardless of internal call sequence. They survive refactors of the implementation. Where determinism is claimed (deep modules are pure), tests use exhaustive enumeration or property-based generation, not fixture freezes. Integration tests use a fake LLM (deterministic stub returning canned `answer` strings) so retrieval + assembly + validation + audit can be tested without provider flakiness.

**Tested deep modules (unit, pure).**

- `CitationParser` — well-formed `[^N]`, malformed, escape sequences, inside code-fence (do not parse), Unicode, very large N, repeated markers, mixed with `[N]` non-citation text.
- `RrfFuser` — rank fusion correctness against known RRF reference (`k=60` standard), K cap, URN attachment, tie-break determinism, empty bucket handling.
- `UrnCodec` — property-based round-trip, special characters in collection names, vector suffix, graph edge URN.
- `PromptAssembler` — golden fixtures for system prompt order, sandbox tag layout, citation directive presence, source content escaping (no `</source>` injection from row content).
- `ProviderCapabilityRegistry` — exhaustive lookup tests; default for unknown provider; capability override via settings.
- `StrictValidator` — branches: ok, retry needed, retry exhausted, lenient warning-only, mode fallback (strict on non-supporting provider → lenient + warning).
- `CostGuardEvaluator` — each individual threshold, multi-tenant isolation, daily cap reset at UTC midnight (deterministic clock injection), zero/negative/overflow edges.
- `AuditRecordBuilder` — every field present and well-typed, `answer_hash` is deterministic SHA-256 of the answer, `include_answer` flag toggles content vs hash, redaction policy applied.

**Tested integration paths.**

- `AskOrchestrator` end-to-end with a fake LLM: strict success, strict failure with retry success, strict failure with retry exhaustion, provider failover triggered by stub 5xx, cache hit, cache miss, RLS-denied source not appearing, prompt injection content not hijacking model (stub returns clean answer), cost guard trip.
- Transport parity: ASK via embedded stdio, HTTP, gRPC, Postgres-wire, MCP — same fake-LLM-backed integration suite produces identical results (schema-wise) on each.
- Cluster behavior: ASK on replica writes audit row visible on primary before answer returns; primary unreachable surfaces 503; cost cap on primary enforces across replicas.

**Prior art in the codebase.**

- Existing parser tests live near `crates/reddb-server/src/storage/query/parser/`; the new `ASK` options syntax follows the same pattern.
- The placeholder parser tests added under #353/#354 are direct prior art for how a deep, pure parser module should be tested.
- Wire codec round-trip tests in `crates/reddb-wire/` are the model for URN codec property-based tests.
- AI provider tests already exist in the `ai` module — extend rather than create parallel harnesses.
- `red_config` integration tests are prior art for how `red_ask_audit` should be exercised end-to-end.

## Out of Scope

- **NLI / entailment / lexical-overlap citation validation.** Structural-only in v1.
- **Multi-step / agentic ASK with tool use.** Single-shot in v1; `ASK '...' STEPS N` is a future extension.
- **Bucket-aware retrieval quotas** (`LIMIT 5 TABLES, 5 VECTORS, 5 GRAPH`). K-only in v1.
- **Adaptive retrieval router** (engine infers intent and chooses strategy). v2.
- **Semantic answer cache** (cosine similarity over question embeddings). v2.
- **Trusted-context mode** (`ASK '...' AS SERVICE`). Deliberate omission — too easy to misuse.
- **Per-source `trust_level`** (e.g. logs=untrusted, internal_docs=trusted). v2 if injection sandbox proves insufficient.
- **Span-level grounding (post-hoc claim → source matching).** v2.
- **Streaming over Postgres-wire.** PG-wire stays non-streaming; HTTP/embedded/MCP support `STREAM`.
- **Public eval / leaderboard / TPC-style benchmark.** Code only; benchmarks come later, when the wedge claim is stable enough to publish numbers against.
- **Formal API versioning commitment.** v1 ships without `api_version` field; promise comes after stabilization.
- **Postgres-wire prepared statements for ASK.** Inherits from #360 once that lands.
- **Migration of every existing `ASK` doc example.** A docs-update issue is created as a child; the PRD only mandates the primary guide updates.

## Further Notes

- This PRD is the AI-native wedge identified in the maturity-gap analysis: arquitetura multi-modelo já compete com Postgres+pgvector+LangChain; o gap real é empacotar o moat. ASK-com-grounding-citações é o demo de 30 segundos que torna o moat visível.
- Aligns with ADR 0010 (`wire-adapters-translate-never-duplicate`): the `AskOrchestrator` is the single source of truth; HTTP, gRPC, RedWire, Postgres-wire, embedded stdio, and MCP all translate to/from its inputs/outputs without duplicating logic.
- Uses `red_*` system-collection convention (per ADR-319) for `red_ask_audit`.
- Builds on Blob Cache infrastructure (ADR 0006) for `CACHE TTL`.
- Builds on RLS + tenancy infrastructure already in `docs/security/`.
- Inherits parameterized-query infrastructure landing in PRD #351 — `ASK` accepts parameterized question text in due course.
- Wedge demos: cybersecurity incident analysis (current `ask-your-database.md` guide), customer churn analysis, RAG-over-internal-docs, and a "live grounded answer" comparison vs Postgres+pgvector that should not be published until evals stabilize.
- Maturity ladder: this PRD delivers the code. Eval/leaderboard/benchmark are deliberately deferred per user direction ("por enquanto sem provas, só código"). When evals come, the architecture above is what they will be measured against.
- A child docs-sweep issue will update `docs/guides/ask-your-database.md`, `docs/guides/rag-in-20-lines.md`, `docs/api/embedded.md`, `docs/api/http.md`, `docs/api/postgres-wire.md`, and `docs/api/mcp.md`.
