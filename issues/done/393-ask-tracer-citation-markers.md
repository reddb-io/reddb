# Tracer: ASK returns [^N] markers + citations array via HTTP (CitationParser) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/393

Labels: enhancement

GitHub issue number: #393

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Foundational tracer bullet for citation grounding. After this slice, an HTTP caller can run:

```
POST /query  {"query": "ASK 'why did X churn?'"}
```

and receive:

- `answer` containing `[^N]` markers placed at factual claims by the LLM
- `citations` array of `{marker, span: [start, end], source_index}` extracted by the server
- `sources` still as buckets (legacy) for backward compat — flat layout comes in #3

Introduces the `CitationParser` deep module (pure: text → spans, indices, errors). Updates the ASK system prompt to instruct LLMs to emit `[^N]` markers. Validation is structural-only with no retry yet (#4 adds retry). No audit, no cache, no cost guards yet — those are subsequent slices.

## Acceptance criteria

- [ ] `CitationParser` deep module is pure, isolated, and has unit tests covering: well-formed `[^N]`, malformed (`[^]`, `[^abc]`, `[^-1]`), escape (`\[^1\]`), inside fenced code blocks (do not parse), Unicode neighbors, repeated markers, very large N.
- [ ] ASK system prompt instructs the LLM to emit `[^N]` markers grounded in retrieved sources.
- [ ] HTTP `/query` response includes new `citations` array.
- [ ] Existing `sources` bucket layout unchanged in this slice.
- [ ] Out-of-range indices appear in `validation.warnings` (no retry; #4 adds retry).
- [ ] Integration test with a fake LLM stub that returns canned answers containing `[^N]`.
- [ ] `docs/guides/ask-your-database.md` shows the new `citations` field.

## Blocked by

- #392
