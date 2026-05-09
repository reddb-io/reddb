# AI batching: EmbeddingDedupCache (LRU opt-in) + TextChunker (single mode) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/277

Labels: needs-triage

GitHub issue number: #277

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Dois módulos add-on sobre `AiBatchClient`. Reduzem custo (dedup) e evitam falhas em texto longo (chunker).

End-to-end:
- **`EmbeddingDedupCache`** em `runtime/ai/dedup_cache.rs`:
  - LRU `BLAKE3(text) -> Vec<f32>` threadsafe (Mutex<lru::LruCache> ou DashMap).
  - TTL configurável.
  - Off por default; opt-in via `runtime.ai.embedding_dedup_enabled = true`.
  - Quando `embed_batch` recebe inputs: lookup cache, marca hits, envia só misses ao provider, mescla resultados na ordem.
  - Configs: `_dedup_ttl_ms`, `_dedup_lru_size`.

- **`TextChunker`** em `runtime/ai/text_chunker.rs`:
  - Interface `chunk(text: &str, max_tokens: usize) -> Vec<String>`.
  - Tokenização aproximada (1 token ≈ 4 chars; tiktoken-rs futuro).
  - Estratégia: parágrafo > sentença > caracter.
  - Modo `Single` (default) — retorna só primeiro chunk (preserva 1:1).
  - Modo `Multi` (opt-in) — múltiplos chunks; comportamento downstream decide concat/avg (futuro).
  - Aplicado dentro de `AiBatchClient::embed_batch` antes de chamar provider.

- Configs: `runtime.ai.embedding_chunk_mode = single|multi`, `runtime.ai.embedding_max_tokens` por provider/model.
- Métricas: `embedding_dedup_hits_total`, `embedding_chunked_total`.

## Acceptance criteria

- [ ] Dedup OFF default — comportamento idêntico ao da slice 3.
- [ ] Dedup ON: 1000 inputs com 10 textos únicos → mock recebe 10 inputs.
- [ ] Cache TTL respeitado: input expirado refeito.
- [ ] Cache LRU evicta conforme `_dedup_lru_size`.
- [ ] Chunker: texto > 8K tokens é chunkado em modo Single → primeiro chunk vai ao provider.
- [ ] Chunker modo Multi gera N chunks (futuro consumer decide concat/avg).
- [ ] Métricas Prometheus expostas.
- [ ] Sem regressão na slice 4 quando ambos ON.

## Blocked by

- #275
