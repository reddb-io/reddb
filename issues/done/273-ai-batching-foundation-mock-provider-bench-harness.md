# AI batching: foundation — mock provider + bench harness [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/273

Labels: needs-triage

GitHub issue number: #273

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Pre-requisite slice. Sem ela, a slice 4 (BIG WIN) não pode validar o speedup. Sem o mock, todas as outras slices precisam mockar manualmente.

End-to-end:
- **Mock AI provider** em `tests/support/mock_ai_provider.rs`: implementa OpenAI-compatible HTTP API. Configurável para retornar 200 OK / 429 / 500 / timeout / texto duplicado / latency artificial. Roda como server HTTP local em test fixture.
- **Bench harness** em `crates/reddb-server/benches/ai_batch_bench.rs` (novo): mede `INSERT 1000 rows WITH AUTO EMBED USING mock` antes/depois. Reusa pattern de `blob_cache_bench.rs`.
- Mock também serve como reference para slices subsequentes.

## Acceptance criteria

- [ ] `MockAiProvider` em `tests/support/` aceita config: `latency_ms`, `error_rate`, `error_kind` (429|500|timeout), `dup_text_count`.
- [ ] Mock expõe contadores: `total_requests`, `total_inputs`, `unique_inputs` para asserts em testes.
- [ ] `cargo bench -p reddb-server --bench ai_batch_bench` roda baseline (1000-row INSERT com legacy per-row pattern).
- [ ] Bench reporta `requests_per_insert`, `total_duration_ms`, `provider_latency_p50/p99`.
- [ ] Slices subsequentes podem importar o mock e usar nos integration tests sem boilerplate.

## Blocked by

None - can start immediately
