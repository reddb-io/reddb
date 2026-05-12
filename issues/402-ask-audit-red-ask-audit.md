# Audit obligatory to red_ask_audit (AuditRecordBuilder) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/402

Labels: needs-triage

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
