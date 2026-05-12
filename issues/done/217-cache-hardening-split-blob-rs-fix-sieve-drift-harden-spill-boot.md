# Cache hardening: split blob.rs, fix SIEVE drift, harden spill + boot [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/217

Labels: enhancement

GitHub issue number: #217

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como mantenedor do cache do RedDB, ao revisar `crates/reddb-server/src/storage/cache/`, encontrei drift entre a implementação SIEVE canônica (`sieve.rs`) e a SIEVE local do blob cache (`blob.rs`), além de problemas de robustez em paths de boot, spill e eviction. O resultado: difícil de evoluir (blob.rs com 5201 linhas mistura 5 responsabilidades), invariante #1 do `cache/README.md` violado sem documentação, panics em I/O recuperável de boot, e um "checksum" no spill que não detecta corrupção real.

Nada disto é bug observável em produção hoje, mas todos comprometem confiança em um subsistema que é o caminho quente do banco.

## Solution

Hardening agregado do módulo `storage/cache/` em fatias verticais independentes:

1. Quebrar `blob.rs` em submódulos coesos com interfaces estáveis.
2. Reconciliar a SIEVE do blob com o invariante #1 do README — documentar exceção formal ou remover prioridade.
3. Trocar O(n) por O(1) no Shard usando `slot_index` como em `sieve.rs`.
4. Substituir `expect()` de I/O em paths de boot por `Result` propagado.
5. Trocar o pseudo-checksum do spill por `crc32` real e sanitizar nomes de arquivo.
6. Mover writeback de `evict_one` para fora dos locks; logar `PoisonError` em vez de silenciar.

## User Stories

1. Como engenheiro lendo `blob.rs` pela primeira vez, quero submódulos por responsabilidade (config, shard, entry, l2, cache) para entender o sistema sem rolar 5000 linhas.
2. Como mantenedor adicionando uma nova política de admissão L1, quero modificar apenas `blob/shard.rs` sem ler o codec L2.
3. Como mantenedor escrevendo testes da SIEVE do Shard, quero o Shard expor uma interface testável isolada (sem montar BlobCache inteiro).
4. Como mantenedor lendo `cache/README.md`, quero que a regra "uma única eviction signal" reflita a realidade do código — ou o blob respeita, ou o README documenta a exceção e o porquê.
5. Como operador rodando carga sustentada no blob cache, quero `Shard::insert/remove` em O(1) para não pagar varredura linear no `Vec<Key>` a cada operação.
6. Como operador subindo o servidor com config L2 inválida, quero erro retornado e log estruturado, não panic do processo inteiro.
7. Como operador depurando crash, quero que `expect()` em paths de boot do BlobCache se torne `Result` propagado para `BlobCache::open` falhar graciosamente.
8. Como operador restaurando spill após crash de host, quero que checksum corrompido seja detectado de fato — não passar permutações de bytes.
9. Como operador com nomes de spill vindos de input semi-confiável, quero que o filename seja sanitizado para impedir path traversal (`../etc/...`).
10. Como mantenedor lendo trace de eviction, quero saber se um lock foi recuperado de poison — não silenciar.
11. Como cliente de leitura concorrente sob pressão, quero que `evict_one` não segure write-locks de `entries` + `slots` durante uma escrita de página.
12. Como time de SRE, quero métricas de health do cache (tempo médio em writeback dentro do lock, taxa de poison recovery) para detectar regressões.
13. Como engenheiro escrevendo testes de regressão, quero um teste que prove que duas inserts concorrentes não bloqueiam pela duração de um writeback.
14. Como engenheiro escrevendo testes de spill, quero um teste que mute 1 byte do arquivo e prove que `read` rejeita.
15. Como engenheiro escrevendo testes de spill, quero um teste que tente `name = "../../etc/foo"` e prove que o filename resultante fica dentro do diretório de spill.
16. Como engenheiro de revisão, quero benchmarks antes/depois para a mudança O(n) → O(1) do Shard, comprovando que ganhamos throughput sob shards grandes.
17. Como autor de ADR, quero que a decisão de eliminar (ou formalizar) a prioridade do Shard seja registrada no documento de arquitetura existente, não em um ADR órfão.
18. Como mantenedor evoluindo o L2, quero que `BlobCacheL2` viva em arquivo próprio com seus testes próximos.
19. Como reviewer de PR, quero que cada fatia do hardening seja independente e mergeável sozinha — não um mega-PR.

## Implementation Decisions

### Módulos a criar / modificar

- **`cache/blob/`** — promovido de arquivo para diretório:
  - `config.rs` — `BlobCacheConfig`, builders, defaults, constantes públicas.
  - `entry.rs` — `Entry`, `L2Record`, codec (encode/decode), checksum.
  - `shard.rs` — Shard SIEVE local com tabela + slots indexados (não `Vec<Key>`).
  - `l2.rs` — `BlobCacheL2`: pager, metadata B+ tree, synopsis (Bloom), control sidecar.
  - `cache.rs` — `BlobCache` orquestrador, sharding, API pública.
  - `mod.rs` — re-exports preservando API atual (`pub use blob::*`).
- **`cache/sieve.rs`** — `evict_one` libera locks antes de chamar `writer.write_page`; `recover_*_guard` loga via tetis-logger pattern em vez de silenciar.
- **`cache/spill.rs`** — checksum migra para `engine::crc32`; helper `sanitize_spill_name(name)` valida que o nome resultante não escapa do diretório base.
- **`cache/README.md`** — invariante #1 reescrito: ou afirma "blob/shard NÃO tem segundo signal" (após remoção da prioridade), ou registra exceção documentada com motivo.

### Decisões técnicas

- **SIEVE do Shard:** decisão pendente entre (a) remover `has_lower_priority_unvisited` e usar SIEVE puro como `sieve.rs`, ou (b) manter prioridade e documentar como variante explícita. Decidir antes de implementar — afeta interface do `Shard`.
- **Boot errors:** `BlobCache::new` torna-se infalível apenas para configs sem L2; `BlobCache::open_with_l2` retorna `Result<BlobCache, CacheError>`. Não introduzir nova variante de erro — estender `CacheError` existente.
- **Poison logging:** usar pattern do `@tetis-lair/tetis-logger` se aplicável, senão `tracing::warn!` com contexto (lock name, thread).
- **API pública preservada:** todos os símbolos exportados em `cache/mod.rs:42-63` continuam acessíveis via re-export. Sem breaking change para consumidores.
- **Sem `git rebase`** durante a série de PRs — fatias landam por merge, conforme regra global do projeto.

### Schema / formato

- Spill on-disk: bumpar versão do header de 1 para 2 quando trocar checksum; reader aceita v1 (legacy fold) por uma janela e v2 (crc32). Decisão sobre janela de compatibilidade durante triagem.
- Sem mudança no formato L2 do BlobCache.

## Testing Decisions

### Princípio

Testar comportamento externo observável: latência sob contenção, integridade pós-corrupção, rejeição de paths inseguros, ordem de eviction. Não testar a estrutura interna do `Shard` (ex: o índice exato do slot ocupado). Match com prior art em `blob.rs` `tests` (linhas 3469-4144) e `sieve.rs` `tests`.

### Cobertura nova

- **`Shard` (deep module):**
  - inserts/removes mantêm SIEVE order esperada.
  - benchmark micro: insert N items, medir tempo p99 — comprovar regressão O(n) → O(1).
  - eviction respeita `pin_count` e ignora prioridade (ou respeita, conforme decisão).
- **`spill` checksum:**
  - round-trip OK retorna bytes idênticos.
  - mutar 1 byte → read retorna erro.
  - permutar 2 bytes → read retorna erro (caso o fold legacy passaria).
- **`spill` path safety:**
  - `name = "../foo"`, `name = "/etc/passwd"`, `name = "a/b"` → arquivo final permanece dentro do diretório base ou erro.
- **`sieve.rs` writeback contention:**
  - cenário: writer bloqueado N ms; observar que insert paralelo não espera o writeback inteiro (mede latência, threshold conservador).
- **`BlobCache::open` falha graciosamente:**
  - L2 path inválido → retorna `Err(CacheError::...)` em vez de panic.

### Prior art

- `cache/blob.rs:3469-4144` — testes de blob (round-trip, eviction, L2 backup).
- `cache/sieve.rs` `mod tests` — concorrência básica.
- `cache/spill.rs` `mod tests` — round-trip.
- `cache/mod.rs:167-369` — testes de archive/restore L2 com `LocalBackend`, padrão a copiar para qualquer novo teste de I/O.

## Out of Scope

- Migração para W-TinyLFU (rastreada em #190 follow-up).
- Mudança no formato L2 do BlobCache.
- Performance tuning além do O(n) → O(1) do Shard (ex: false sharing, hugepages).
- Rework do `result.rs`, `aggregates.rs`, `extended_ttl.rs`, `promotion_pool.rs` — fora do achado do review.
- Auditoria de `validate_metadata` (`blob.rs:2703`) em profundidade — possível PRD futuro.
- Documentar ADR em arquivo novo sob `docs/adr/` (regra do projeto: embedar no documento que readers já visitam, ex: `cache/README.md`).

## Further Notes

- Origem: review de qualidade do cache em 2026-05-07. 7 problemas top, este PRD agrega 6 deles (split + invariante + O(n) + boot + spill + writeback/poison).
- Problema "spill checksum" tem componente de segurança — priorizar essa fatia se entrar release.
- Após este PRD, rodar `to-issues` para fatiar em tracer-bullets independentes; cada fatia deve ser mergeável e testável isoladamente.
- Não criar `docs/adr/cache-hardening.md` — atualizar `cache/README.md` (regra de memória do usuário: sem ADRs órfãos).
- Para benchmarks de regressão, reusar harness existente em `crates/reddb-server/benches/blob_cache_bench.rs` (#190).
