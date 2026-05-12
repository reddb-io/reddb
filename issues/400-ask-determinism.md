# Determinism: temperature=0 + seed=hash + overrides [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/400

Labels: needs-triage

GitHub issue number: #400

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Default `temperature=0` and `seed = hash(question + sources_fingerprint)` for providers that support it.

Per-query overrides: `ASK '...' TEMPERATURE 0.7 SEED 42`. Settings: `ask.default_temperature`.

Sources fingerprint is a stable hash over the URNs and content versions of the retrieved sources — ensures that same question + same data yields same seed even when no cache is involved.

Providers without seed support (per #396) use temperature only; audit row records what was actually sent.

## Acceptance criteria

- [ ] Default `temperature=0` applied to every ASK call.
- [ ] Default `seed` derived from question + sources_fingerprint for supporting providers.
- [ ] Per-query `TEMPERATURE` and `SEED` overrides parse and apply.
- [ ] Setting `ask.default_temperature` honored.
- [ ] Integration test: same question, same data, two calls → same answer string (within provider determinism guarantees) on OpenAI/Groq stubs.
- [ ] Audit row records the seed and temperature actually used.

## Blocked by

- #393
