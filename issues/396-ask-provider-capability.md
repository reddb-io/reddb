# Provider capability registry + graceful fallback (ProviderCapabilityRegistry) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/396

Labels: enhancement

GitHub issue number: #396

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Introduces the `ProviderCapabilityRegistry` deep module — pure lookup of `provider → {supports_citations, supports_seed, supports_temperature_zero, supports_streaming}`.

When strict mode is requested against a provider whose `supports_citations` is false (e.g. local small models), ASK transparently falls back to lenient mode and surfaces a `mode_fallback` warning. Audit row records the actual mode used.

The registry covers all 11 currently-supported providers (OpenAI, Anthropic, Groq, OpenRouter, Together, Venice, DeepSeek, HuggingFace, Ollama, Local, Custom URL). Settings allow per-deployment overrides.

## Acceptance criteria

- [ ] `ProviderCapabilityRegistry` deep module with exhaustive unit tests per provider.
- [ ] Strict + non-supporting provider → lenient + warning, not error.
- [ ] Setting `ask.providers.capabilities.<name>` can override built-in flags.
- [ ] Unknown provider returns conservative defaults (no citation support, no seed).
- [ ] Integration test covering fallback path with a stub provider marked as non-supporting.

## Blocked by

- #395

## Progress

Slice 1: `ProviderCapabilityRegistry` deep module landed at
`crates/reddb-server/src/runtime/ai/provider_capabilities.rs` with 20
unit tests covering every branch. Pure — no I/O, no clock, no LLM
calls. Exposes:

- `Capabilities { supports_citations, supports_seed,
  supports_temperature_zero, supports_streaming }` with
  `for_provider(token)` built-in rows and `conservative()` for unknown
  tokens (AC: "Unknown provider returns conservative defaults").
- `Registry` holds an optional per-deployment override map, lower-cased
  on insert and lookup. `with_override` replaces the row wholesale
  (matches the one-TOML-table-per-provider settings surface planned
  in #401).
- `Mode` is reused from `strict_validator` so the registry's decision
  composes with #395's policy.
- `evaluate_mode(token, requested) -> ModeOutcome::{Allowed, Fallback}`
  implements the AC's strict→lenient fallback when the provider's
  `supports_citations` is `false`, with a `mode_fallback` warning
  carrying the provider token in `detail`.

Tests pin:
- conservative defaults (citations/seed off, temp0 on, streaming off);
- per-provider rows for OpenAI / Anthropic (no seed) / the OpenAI-
  compatible family (Groq, Together, OpenRouter, Venice, DeepSeek) /
  Ollama (no citations, but seed + streaming) / HuggingFace (raw
  inference: no seed, no streaming) / Local (no temperature at all) /
  Custom (conservative);
- lenient always passes through, regardless of provider;
- strict on a citing provider stays strict;
- strict on a non-citing OR unknown provider downgrades to lenient
  with `ModeWarningKind::ModeFallback`;
- overrides can both upgrade (Ollama→citing) and downgrade
  (OpenAI→non-citing);
- determinism on repeated calls;
- the 11-provider matrix (`AiProvider` enum) is exhaustively pinned —
  adding/removing a provider in `ai.rs` will force this test to
  update.

Deferred to follow-up slices:

- Wire `evaluate_mode()` into `execute_ask` before calling
  `StrictValidator`, threading `ModeOutcome.warning()` into the
  response envelope.
- Surface `ask.providers.capabilities.<name>` overrides in runtime
  config (TOML/KV plumbing identical to #401 settings).
- Record the *effective* mode in the audit row (#402).
- Integration test with a stubbable LLM provider marked as
  non-citing — depends on the transport-stubbing refactor that's
  also blocking #395's integration test.

Deep module is the load-bearing piece; remaining slices are mechanical
wiring and can land independently. Issue stays open with this progress
note (mirrors slice 1 pattern of #395 and #401).
