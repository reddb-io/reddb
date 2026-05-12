# Provider failover ordered list configurable [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/404

Labels: needs-triage

GitHub issue number: #404

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Ordered provider failover triggered on transport errors, 5xx, or timeout.

Settings: `ask.providers.fallback = ['groq', 'openai', 'anthropic']`. Per-query override: `ASK '...' USING 'groq,openai'`.

Failover preserves seed, temperature, and strict mode across attempts. The successful provider is recorded in the response `provider` field and audited. If all providers fail, return 503 with a list of attempted providers and their errors.

## Acceptance criteria

- [ ] Failover triggers on 5xx, transport errors, and timeout.
- [ ] Per-query `USING 'a,b,c'` overrides global setting.
- [ ] Successful provider surfaced in response and audit.
- [ ] All-providers-failed produces 503 with attempt list.
- [ ] Seed and temperature preserved across failover attempts.
- [ ] Integration test with two stub providers where the first errors and the second succeeds.

## Blocked by

- #396
