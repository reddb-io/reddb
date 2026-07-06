# ADR 0068 — ASK planner-first redesign

**Status:** Accepted
**Date:** 2026-07-04
**Supersedes:** ADR 0013 §15 (MVP single-shot, no agentic loop); amends ADR 0013 (ASK contract) and ADR 0057 (credential path shape)

## Context

ADR 0013 shipped `ASK` as single-shot RAG: retrieve → synthesize → cite. The product
vision, however, is query-first AI: "o que aconteceu com o passaporte 123?" should
*find the collections that hold a passport attribute, generate the query (joins
included), execute it, and answer from the result*. Today that exists only as the
deterministic `AS RQL` planner (single `field = literal`, no LLM, opt-in `EXECUTE`).
The gap between the shipped contract and the vision prompted this redesign.

## Decisions

### 1. ASK becomes planner-first

Every `ASK 'question'` runs a **planning step** before anything else:

1. **Deterministic funnel first** (the existing AskPipeline: tokens → SchemaVocabulary
   → BM25/vector/graph, RLS-scoped) narrows the schema slice. The LLM never sees the
   raw catalog — with 1000 collections it would drown in metadata; the funnel is the
   anti-drowning gate.
2. **LLM planner second**, on the narrowed slice only. The planner call folds in a
   self-critique ("does this grounding sustain the question?") and may emit at most
   **one** `refine_retrieval` retry — mirroring ADR 0013's single citation retry.
3. The planner emits a **typed plan** (steps: retrieve / query / synthesize) validated
   by the production parser before execution. Step budget: `red.config.ai.ask.max_plan_steps`
   (default 3); per-query `STEPS N` never exceeds the config cap.

### 2. Three intents, routed by the planner

- **Factual** ("what happened to passport 123?") — generate read-only RQL over the
  **full read-only surface** (SELECT incl. global `any`, joins, aggregations,
  graph/vector/search), **auto-execute by default**, then synthesize a natural-language
  answer with validated `[^N]` citations *over the executed rows* (the executed
  result set becomes `sources_flat`; ADR 0013's grounding machinery is reused intact).
- **Synthesis** ("summarise yesterday's incidents") — retrieval-RAG with cited answer,
  as ADR 0013 ships today.
- **How-to** ("how would I capture events from a collection into a queue?") — the
  answer carries a structured `suggestion` field: parser-validated statements each
  flagged `mutating: true|false`, plus rationale. Suggested statements — including
  DDL/mutating ones — are **returned, never executed**. Materializing a suggestion as
  a migration artifact is a separate future command; ASK itself has no write
  side-effects.

### 3. Execution contract (clean break)

- Read-only generated queries **execute by default**. The `EXECUTE` and `AS RQL`
  clauses are removed — no deprecation window, per house rule. A plan/query-only
  parameter returns the plan and candidate query without executing.
- A mutating candidate is **never** auto-executed under any flag; it can only travel
  in the `suggestion` envelope.
- The planner LLM has its own config slot (`red.config.ai.ask.planner_model`, effort,
  etc.) so a cheap/fast model can plan while the user-chosen model synthesizes.

### 4. Provider posture — the user chooses

Two deployment scenarios, one rule: the user picks local (Ollama; HF Inference API)
or SaaS. On reddb.io cloud, the platform injects an org-level **OpenRouter** token at
the `default` credential alias; tenants use it transparently, may override with their
own token under their own alias, and **can never read the platform value** — verified
structural guarantees: role-independent `red.secret.*` hard-block in the `$secret`
resolver, keys stored outside any collection, prompt-level secret redaction, audit
carries paths never values. Cloud injection additionally ships non-admin tenant
principals, an explicit Deny policy on the platform alias path, and `policy_only`
enforcement. ADR 0057's "no in-process model runtime" stance stands.

### 5. AI config/secret namespace (clean break)

```
red.secret.ai.providers.$provider.tokens.$alias         # vault-only; alias "default" implicit
red.config.ai.ask.{provider,model,planner_model,effort,max_plan_steps,…}
red.config.ai.providers.$provider.{base_url}
red.config.ai.providers.$provider.models.{inference,embeddings}
red.config.ai.inference.provider                        # which provider generates
red.config.ai.embeddings.provider                       # which provider embeds
```

Resolution: ASK-specific → task pointer → provider block. A task whose pointed
provider lacks the modality fails with a didactic error — no silent re-route (the
Anthropic-embeddings rule generalized). The legacy plaintext path
`red.config.ai.<provider>.<alias>.key` is **removed**; resolution rejects it with an
error naming the vault path.

## Rejected, and why

- **Keep ASK RAG-only and grow `AS RQL` on the side.** Rejected — the product vision
  is query-first; two half-features under one keyword confuse the contract.
- **LLM planner from the first token (no funnel).** Rejected — a large catalog drowns
  the model in metadata; the deterministic funnel is what makes 1000 collections viable.
- **Heuristic-only planner.** Rejected — it cannot route ambiguous intent nor compose
  joins; that ceiling is why the current `AS RQL` feels shallow.
- **Unbounded agentic loop (cost-guard only).** Rejected — worst-case latency becomes
  invisible to the operator; the hard step cap keeps cost, latency, and audit
  linearization predictable.
- **Renaming namespaces to plural (`red.secrets.*`) / dropping credential aliases.**
  Rejected — singular is the established security-hard-block namespace, and the alias
  is precisely what separates the platform credential from tenant overrides with
  path-separable read/write policies.
- **Top-level task blocks with `{provider,model}`.** Rejected — duplicates the model
  choice that lives with the provider; two sources of truth.
- **ASK materializing migration drafts in the database.** Rejected — a question
  command with write side-effects surprises and complicates RLS/audit; suggestion
  envelope first, apply-command later.

## References

- ADR 0013 — ASK grounding citations (contract amended; §15 superseded)
- ADR 0057 — AI multi-modality spine (credential-home shape amended; local-runtime stance reaffirmed)
- Glossary: `.red/context/data-model.md` (Ask planner / Ask intent / Ask auto-execution rule / Ask suggestion), `.red/context/governance.md` (AI credential/config namespace, Platform AI credential)
