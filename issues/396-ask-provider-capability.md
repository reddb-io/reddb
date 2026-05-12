# Provider capability registry + graceful fallback (ProviderCapabilityRegistry) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/396

Labels: needs-triage

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
