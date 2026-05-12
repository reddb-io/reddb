# ADR: ASK with inline citations — format, addressing, validation, cluster, audit [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/392

Labels: enhancement

GitHub issue number: #392

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

ADR `00XX-ask-grounding-citations.md` codifying the design decisions for ASK with inline grounding citations. This is the contract every subsequent slice implements against.

The ADR must decide and document:

- Inline citation format: `[^N]` markers, 1-based, positional, repeatable. Behavior inside code-fences and string literals.
- Source addressing: flat `sources` array. Each entry carries `kind`, `urn`, `content`, `score`, plus kind-specific extras.
- URN scheme: `reddb:<collection>/<id>`, with suffix conventions for vectors (`#<id>`), graph edges (`#<edge_id>`), document fragments.
- Strict-mode policy: structural validation only in v1; one retry on failure; lenient is opt-in via `STRICT OFF`; per-provider capability fallback.
- Prompt-injection defense: sources wrapped in `<source>` tags with explicit system prompt that source content is data.
- Determinism: `temperature=0` default, `seed = hash(question + sources_fingerprint)` for providers that support it.
- Cost guards: configurable settings (`ask.max_prompt_tokens`, etc.); 413/504 on exceed.
- Audit policy: obligatory write to `red_ask_audit`; answer not stored by default; retention configurable.
- Cluster behavior: ASK on replicas; audit + cost forwarded synchronously to primary before answer returns; cache populate async.
- Provider failover: ordered list configurable; per-query override; preserves seed within a call.
- Streaming: non-stream default; opt-in `STREAM` over HTTP/embedded/MCP only.
- Out-of-scope for v1: NLI/entailment, multi-step/agentic, semantic cache, span-level grounding, API version commitment.

## Acceptance criteria

- [ ] ADR file added under `docs/adr/` with the next sequential number.
- [ ] All decisions above are explicitly stated.
- [ ] ADR status: Accepted.
- [ ] PRD #391 references the ADR number.

## Blocked by

None - can start immediately.
