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

## Progress

Slice 1: `DeterminismDecider` deep module landed at
`crates/reddb-server/src/runtime/ai/determinism_decider.rs` with 19
unit tests. Pure — no I/O, no clock, no LLM calls. Exposes:

- `Inputs { question, sources_fingerprint }` — opaque fingerprint
  (decider doesn't recompute it; the retrieval layer owns the format).
- `Overrides { temperature: Option<f32>, seed: Option<u64> }` parsed
  from `ASK '...' TEMPERATURE x SEED n`.
- `Settings { default_temperature }` with `Default::default()` →
  `0.0` (the spec default).
- `Applied { temperature, seed }` — exactly what the caller should
  send to the provider AND what the audit row should record.
- `decide(inputs, caps, overrides, settings) -> Applied`.
- `derive_seed(question, fingerprint) -> u64` — `sha256(question ||
  0x1f || fingerprint)`, first 8 bytes little-endian. 0x1f (ASCII US)
  is used as the field delimiter to keep the concatenation injective
  without escaping; `derive_seed_is_injective_across_field_boundary`
  pins that `("ab","c") != ("a","bc")`.

Policy pinned by tests:
- temperature: override > settings.default > 0.0; if
  `caps.supports_temperature_zero == false` (Local-class endpoints
  that take no temperature at all), `temperature` is dropped to
  `None` even when overridden;
- seed: if `caps.supports_seed == false` (Anthropic, HuggingFace,
  Local, Custom — per #396), `seed` is dropped to `None` whether
  derived or overridden, so the audit row never lies about what the
  provider got;
- `Some(0.0)` and `Some(0)` overrides are preserved (guards against
  `unwrap_or(0)` regressions where override and default would be
  indistinguishable);
- determinism: `decide` and `derive_seed` are byte-equal across calls
  given the same inputs (`decide_is_deterministic_across_calls`,
  `derive_seed_is_deterministic_across_calls`).

Deferred to follow-up slices (each independently shippable):

- Parse `ASK '...' TEMPERATURE x SEED n` in the SQL parser and thread
  `Overrides` into `AskQuery`.
- Surface `ask.default_temperature` in runtime config (TOML/KV
  plumbing identical to #401 settings).
- Compute `sources_fingerprint` in the retrieval layer (likely
  `sha256` over the URN+content-version tuples post-fusion in #398)
  and pass it into `decide`.
- Wire `decide()` into `execute_ask`, set `temperature`/`seed` on the
  provider request, and record `Applied` in the audit row (#402).
- Integration test against OpenAI/Groq stubs verifying same question
  + same data → byte-equal answer (depends on the stubbable LLM
  transport refactor already deferred by #395/#396).

Deep module is the load-bearing piece; remaining slices are
mechanical wiring and can land independently. Issue stays open with
this progress note (mirrors slice 1 pattern of #395, #396, #398,
#401).
