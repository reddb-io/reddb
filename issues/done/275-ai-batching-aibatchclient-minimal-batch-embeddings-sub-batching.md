# AI batching: AiBatchClient minimal (batch embeddings + sub-batching) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/275

Labels: enhancement

GitHub issue number: #275

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Camada de batching sobre `AiTransport`. Aceita `Vec<String>` de inputs, fragmenta respeitando max_batch_size do model, executa requests, reagrupa resultados na ordem original.

End-to-end:
- Novo módulo `runtime/ai/batch_client.rs` com `AiBatchClient`:
  - `embed_batch(provider: AiProvider, model: String, texts: Vec<String>) -> Result<Vec<Vec<f32>>>`.
  - Fragmenta `texts` em sub-batches de até `max_batch_size` (default OpenAI 2048; demais 256-1024).
  - Para cada sub-batch: monta `OpenAiEmbeddingRequest` com `inputs: <sub-batch>`, envia via `AiTransport`.
  - Reagrupa todos resultados em ordem `[0..N-1]` matching inputs original.
  - Empty texts são skipados; reflete null/zero-vector no resultado conforme decisão por slice.
- Sem dedup, sem chunking nesta slice (vão na slice 5).
- Integration tests usando mock provider.

## Acceptance criteria

- [ ] `embed_batch(provider, model, vec!["a", "b", "c"])` retorna `Vec<Vec<f32>>` com 3 entries em ordem.
- [ ] 1000 inputs → mock recebe 1 request (max_batch=2048) ou múltiplas se max_batch<1000.
- [ ] Empty string em input → skipado, resultado mantém posição com vetor vazio ou null.
- [ ] Erro do provider (após retries) propagado claramente, sem partial commit.
- [ ] Configs: `runtime.ai.embedding_max_batch_size` por provider.
- [ ] Integration test: assert `mock.total_requests == 1` para 1000 inputs com OpenAI mock.

## Blocked by

- #274
