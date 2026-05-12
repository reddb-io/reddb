# AI provider batching + resilience: stop per-row embed requests, add retry/pool/dedup/chunking [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/272

Labels: enhancement

GitHub issue number: #272

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como operador rodando `INSERT … VALUES (...), (...), (...) WITH AUTO EMBED` com 1000 linhas, cada linha vira **uma request HTTP separada** ao provider de embeddings. Confirmação no código: `runtime/impl_dml.rs:608-632` é um loop `for entity in &recent` que chama `openai_embeddings` com `inputs: vec![combined.clone()]` — sempre um único item por request.

Consequências práticas:
- **Latência:** 100-500ms por request × 1000 rows = 100-500 segundos só de embeddings.
- **Rate limit:** OpenAI 429 (3000 RPM em tier baixo) trava antes de terminar.
- **Custo de tokens:** mesmos tokens cobrados, mas overhead de header/handshake repetido em cada chamada.
- **Sem retry:** primeiro 429/5xx mata o INSERT inteiro.
- **Possivelmente blocking:** não confirmei async, mas o padrão sugere thread bloqueada por IO.
- **Sem connection pool:** cada call provavelmente cria nova conexão TCP.
- **Sem dedup:** 1000 rows com mesmo texto = 1000 requests com mesmo input.
- **Sem chunking:** texto > 8K tokens em um row é rejeitado pelo provider, sem tratamento.

A request struct **já é batch-capable** — `OpenAiEmbeddingRequest.inputs: Vec<String>` (`crates/reddb-server/src/ai.rs:68`). OpenAI aceita até 2048 strings por request em `text-embedding-3-small`. Os 7 providers OpenAI-compatible (Groq, Together, OpenRouter, Venice, DeepSeek, Ollama, OpenAI) suportam o mesmo. Anthropic é prompts-only e não é afetado.

Speedup estimado em INSERT bulk: **100-500x** (1 request em vez de 1000).

## Solution

Refactor da pipeline de comunicação com AI providers em 4 camadas:

1. **Batch coletor em pipelines de dados** — `INSERT WITH AUTO EMBED`, bulk import (jsonl/parquet com auto-embed se aplicável), ALTER REBUILD INDEX, HTTP bulk endpoints. Cada pipeline acumula textos antes de chamar o provider, faz uma request de até 2048 inputs, distribui embeddings de volta para as entities corretas.

2. **Resilience layer** — retry com exponential backoff em 429/5xx, circuit breaker, timeouts, observability (metrics: latency, retries, dedupe hits).

3. **Connection pool + async** — `reqwest::Client` compartilhado entre calls com pool de conexões, async tokio nativo (sem bloquear thread).

4. **Dedup + chunking** — LRU cache `hash(texto) → Vec<f32>` evita recalcular embeddings duplicadas. Chunker quebra texto longo respeitando max_tokens do modelo.

Resultado:
- INSERT bulk com AUTO EMBED: 100-500x speedup.
- Resiliência: workload de 100k rows não morre em 1 transient 429.
- Custo: identico (mesmos tokens) ou menor (com dedup).
- Compatibilidade: todos os 8 providers seguem o mesmo path; Anthropic continua prompt-only.

## User Stories

1. Como operador rodando INSERT bulk com `WITH AUTO EMBED (description) USING openai`, quero que 1000 rows sejam embedidas em 1-3 segundos (não 5-10 minutos), porque o provider aceita batch e nós deveríamos usar.
2. Como operador, quero que `INSERT INTO articles (body) VALUES (...), (...), (...) WITH AUTO EMBED` envie uma única request HTTP com array de inputs, para minimizar latência de transação.
3. Como operador, quero que se um row tiver texto vazio ou nulo, ele seja **pulado** no batch sem abortar os outros, para não perder o INSERT inteiro por causa de 1 row ruim.
4. Como operador, quero que se o batch exceder 2048 inputs (limite OpenAI), o coletor automaticamente fragmente em sub-batches e faça N requests, para suportar bulk arbitrário.
5. Como operador, quero que se o provider retornar 429 (rate limit), o sistema espere com exponential backoff e tente novamente até 3x, para não abortar workload por congestionamento transient.
6. Como operador, quero que se o provider retornar 5xx, o sistema retry com backoff (mesma política que 429), para resiliência contra falhas momentâneas.
7. Como operador, quero que após 3 tentativas falhadas, o erro seja propagado claramente com `provider`, `status_code`, `attempt_count`, `total_wait_ms` para diagnóstico.
8. Como operador, quero que se 1000 rows tiverem o mesmo texto (deduplicação possível), o sistema faça 1 request e replique o embedding para todas as N entities, para reduzir custo.
9. Como operador, quero ligar/desligar dedup via config (`runtime.ai.embedding_dedup_enabled = true|false`), porque alguns workloads não se beneficiam.
10. Como operador, quero que o cache de dedup tenha TTL configurável (`runtime.ai.embedding_dedup_ttl_ms`) e tamanho máximo (`runtime.ai.embedding_dedup_lru_size`) para evitar memory bloat.
11. Como operador rodando carga sustentada, quero que conexões HTTP sejam reusadas entre requests (connection pool) para evitar overhead de handshake TCP/TLS repetido.
12. Como operador, quero que chamadas a embeddings rodem async (não bloqueiem thread tokio), para que outros INSERT/SELECT continuem rodando em paralelo.
13. Como operador, quero que se um texto exceder o max_tokens do modelo (ex: 8K para `text-embedding-3-small`), o sistema chunke o texto e gere múltiplos embeddings (ou rejeite com erro claro), para não receber 400 do provider.
14. Como operador, quero métricas de latency_ms, retry_count, dedup_hit_rate, batch_size_p99 expostas via `/metrics` (Prometheus), para monitorar saúde da integração AI.
15. Como autor de SDK, quero que requests batch tenham timeout sensato (default 30s, configurável), para não pendurar o cliente em casos extremos.
16. Como engenheiro de SRE, quero log estruturado em cada batch: provider, model, batch_size, total_tokens, duration_ms, retries, para auditoria/debug.
17. Como engenheiro auditando custo, quero ver via stats `total_provider_requests`, `total_batched_inputs`, `total_dedup_hits`, para calcular ROI da feature.
18. Como autor de policy, quero capability `ai:provider:call` que gate quem pode invocar embeddings (não cobrado nesta PRD; flag pra futuro).
19. Como engenheiro mantendo o código, quero um único módulo `AiBatchClient` reusado em todos os call sites (INSERT WITH AUTO EMBED, bulk import, ALTER REBUILD), para que melhorias futuras propaguem automaticamente.
20. Como autor de teste, quero mock provider que simula 429/5xx/timeout/duplicate-text para validar comportamento sem chamar API real.
21. Como engenheiro de plataforma, quero mesmo path async/pool ser usado pra prompts (chat completion), porque o bug de blocking não é exclusivo de embeddings — é arquitetural.
22. Como engenheiro investigando regressão de performance, quero benchmark documentado: "INSERT 1000 rows com AUTO EMBED" antes/depois, para detectar futura regressão.
23. Como autor de release notes, quero que a PRD entrega um speedup mensurável (ex: ≥50x em workload de referência), para destacar como melhoria.

## Implementation Decisions

### Módulos novos / modificados

- **`AiBatchClient` (deep module novo)** — interface única `embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>>` e `prompt(text: String) -> Result<String>`. Internamente:
  - Aplica chunking (via `TextChunker`).
  - Aplica dedup (via `EmbeddingDedupCache`).
  - Fragmenta em sub-batches respeitando model max (ex: 2048 inputs OpenAI).
  - Envia request via `AiTransport` (com retry, pool, timeout).
  - Reordena resultados para casar com input order.
  - Emite métricas + audit log.

- **`AiTransport` (deep module novo)** — wraps `reqwest::Client` compartilhado. Interface única `request(builder) -> Response`. Aplica:
  - Connection pool (default 32 conexões/host).
  - Timeout configurável (default 30s).
  - Retry com exponential backoff em 429/5xx/timeout (default 3 tries, base 500ms × 2^n).
  - Circuit breaker opcional (futuro).

- **`EmbeddingDedupCache` (deep module novo)** — LRU `BLAKE3(text) -> Vec<f32>`. Threadsafe via Mutex/DashMap. TTL configurável. Disabled por default; opt-in via config. Quando `embed_batch` recebe inputs, primeiro consulta cache, marca hits, envia só misses ao provider, fundi resultados.

- **`TextChunker` (deep module novo)** — interface `chunk(text: &str, max_tokens: usize) -> Vec<String>`. Tokenização aproximada (1 token ≈ 4 chars como heurística; tiktoken-rs futuro). Estratégia: partir em parágrafos > sentenças > caracter. Dois modos:
  - `Single` — retorna primeiro chunk apenas (default; preserva 1:1 mapping).
  - `Multi` — gera múltiplos embeddings, concatenados ou averaged (decisão por slice).

- **Modificações em call sites:**
  - `runtime/impl_dml.rs` — AUTO EMBED collector pattern: acumula textos de todos rows do INSERT, chama `AiBatchClient::embed_batch` uma vez, distribui resultados por entity_id.
  - `storage/import/jsonl.rs` + `parquet.rs` — se import config tem `auto_embed`, mesmo collector pattern (não hoje; é seam pra futuro).
  - HTTP bulk endpoints (`/collections/{name}/bulk/rows`) — se body tem `auto_embed_request`, batch.
  - `ai.rs` — `openai_embeddings` deprecated em favor de `AiBatchClient`. Função antiga mantida durante migração.

### Decisões técnicas

- **Default batch size por provider:** OpenAI 2048, demais OpenAI-compatible 256-1024 (conservador inicialmente, ajustável via config).
- **Default retry policy:** 3 tries, base 500ms, exp factor 2.0, cap 10s. Retry só em 429/5xx/timeout/connection-refused.
- **Dedup default:** OFF. Configurável via `runtime.ai.embedding_dedup_enabled`.
- **Async:** todo path AI vira `async fn`, awaited do INSERT executor. Calls síncronas legacy mantidas com bridge `block_on` durante migração.
- **Connection pool default:** 32 connections per (provider, host). Configurável.
- **Timeout default:** 30s. Configurável por provider via `runtime.ai.<provider>.timeout_ms`.
- **Auth:** capabilities ai:provider:call ficam fora desta PRD. Decisão: implementar sem capability check no MVP; PRD futura adiciona.
- **Backward compat:** `openai_embeddings` síncrono mantido como wrapper sobre `AiBatchClient` durante 1 release. Deprecation warning emitido.

### Schema / formato

- Sem mudança em schema persistido.
- Métricas Prometheus novas:
  - `reddb_ai_provider_requests_total{provider,model,status}`
  - `reddb_ai_provider_request_duration_ms{provider,model}`
  - `reddb_ai_provider_retries_total{provider,reason}`
  - `reddb_ai_embedding_dedup_hits_total`
  - `reddb_ai_embedding_batch_size{provider}` (histogram)

### Config novos (runtime.ai.*)

- `runtime.ai.embedding_max_batch_size`
- `runtime.ai.embedding_dedup_enabled`
- `runtime.ai.embedding_dedup_ttl_ms`
- `runtime.ai.embedding_dedup_lru_size`
- `runtime.ai.transport_pool_size`
- `runtime.ai.transport_timeout_ms`
- `runtime.ai.transport_retry_max_attempts`
- `runtime.ai.transport_retry_base_ms`

## Testing Decisions

### Princípio

Testar comportamento externo: "INSERT 1000 rows com AUTO EMBED faz N requests ao provider mock" (não O(rows)). "Provider retornando 429 dispara retry com backoff observável." Não testar implementação interna do `AiBatchClient` (estrutura de cache, ordem de chunks).

### Cobertura nova

- **Mock provider** em `tests/support/`: implementa OpenAI-compatible API, configurável para retornar 429/5xx/timeout/duplicate-text/sucesso. Reusado em todas integration tests.
- **Integration test**: `INSERT … VALUES (..1000 rows..) WITH AUTO EMBED USING <mock>` faz exatamente 1 request com 1000 inputs (ou N requests respeitando max_batch).
- **Retry test**: mock retorna 429 nas primeiras 2 chamadas + sucesso na 3ª; INSERT completa; metrics mostram 2 retries.
- **Dedup test**: 1000 rows com 10 textos únicos + cache habilitado → mock recebe 10 inputs (não 1000).
- **Chunking test**: row com texto > 8K tokens é chunked; ou é rejeitado com erro claro.
- **Timeout test**: mock atrasa > 30s → erro `RequestTimeout` em `total_wait_ms = 30s`.
- **Connection pool test**: 100 INSERTs paralelos reusam ≤ 32 conexões TCP (mocked via `socket count`).
- **Bench**: `INSERT 1000 rows` antes vs depois — alvo ≥50x speedup com mock latency 100ms.
- **Backward compat test**: `openai_embeddings` direto continua funcionando (1 input).

### Prior art

- `tests/support/mod.rs` — fixtures e mocks existentes.
- `crates/reddb-server/src/ai.rs::tests` — unit tests de parsing de provider/api_base.
- `crates/reddb-server/benches/blob_cache_bench.rs` — pattern de criterion bench (reusar para AI bench).

## Out of Scope

- **Capability `ai:provider:call`** para auth de embeddings — PRD futura.
- **Streaming responses** (chat completion stream) — fora de scope; embeddings não streamam.
- **Embedding versioning / migration** quando model muda — PRD futura.
- **Local provider integration** (sentence-transformers em-process) — Ollama cobre isso parcialmente; nativo fica para PRD separada.
- **Multimodal embeddings** (image, audio) — só texto nesta PRD.
- **Quota / billing por tenant** — fora de scope.
- **Webhook ou async result delivery** — INSERT WITH AUTO EMBED continua síncrono do ponto de vista do client.
- **Mudança no DSL** (`WITH AUTO EMBED (...)`) — sintaxe permanece igual.
- **Dedup global cross-collection** — dedup é per-collection ou per-process; não-distribuído.

## Further Notes

- Origem: pergunta do user em 2026-05-08 sobre comunicação com AI providers. Confirmado empiricamente em `runtime/impl_dml.rs:608-632`.
- Sem `git rebase` durante implementação (regra global).
- Sem ADR — esta PRD é refactor de implementação, sem decisão arquitetural nova. As decisões (batch, retry, pool, dedup) são padrões da indústria, não trade-offs únicos do RedDB.
- Estimativa total: 6-9 dias se sequencial. Slices podem rodar em paralelo após `AiBatchClient` estar pronto.
- Após PRD: `to-issues` para fatiar em ~8-10 slices.
- Bench-driven: bench em `crates/reddb-server/benches/ai_batch_bench.rs` (novo) prova o speedup. Sem regressão de bench = release blocker.
- Pareada arquiteturalmente com #239 (catalog) e #240 (column policy) — todas são "limpar gap fundamental no produto" e merecem priorização similar.
