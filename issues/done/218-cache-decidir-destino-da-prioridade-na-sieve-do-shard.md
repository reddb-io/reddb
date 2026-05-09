# Cache: decidir destino da prioridade na SIEVE do Shard [DONE]

GitHub: https://github.com/reddb-io/reddb/issues/218

Labels: enhancement, ready-for-human

GitHub issue number: #218

Decision comment: https://github.com/reddb-io/reddb/issues/218#issuecomment-4412858597

## Status

Decision recorded. The Blob Cache shard keeps priority as a documented
exception to the page-cache SIEVE invariant.

## Original GitHub Body

## Parent

#217

## What to build

Decisão arquitetural HITL: a SIEVE do `Shard` em `blob.rs:1030` introduz `has_lower_priority_unvisited` (linhas 1057/1092), violando o invariante #1 do `cache/README.md` ("uma única eviction signal"). Decidir entre:

(a) **Remover prioridade** — Shard usa SIEVE puro, igual `sieve.rs::PageCache`. Simplifica e respeita o README.
(b) **Formalizar exceção** — manter prioridade documentando o motivo (ex: tiering L1/L2 exige), e reescrever invariante #1 com a exceção explícita.

Output: comentário no issue com decisão + uma linha registrável em `cache/README.md`. Não há código nesta fatia — apenas decisão que destrava slices 7 e 9.

## Acceptance criteria

- [x] Decisão registrada em comentário do issue: (a) remover ou (b) formalizar.
- [x] Se (a): N/A — decisão foi (b), formalizar exceção.
- [x] Se (b): rascunho da nova redação do invariante #1 do `cache/README.md`.
- [x] Justificativa cita workload-alvo (blob L1 com tiers vs page cache homogêneo).

## Blocked by

None - can start immediately

## Decision

Chosen option: **(b) formalizar exceção**.

Keep the Blob Cache shard priority behavior. `sieve.rs::PageCache` remains pure
SIEVE for the homogeneous database page-cache workload, where `visited` is the
only eviction signal. `blob::Shard` is the exception because Blob Cache L1
protects user-visible cached blobs and is designed for tiered L1/L2 cache
workloads with richer policy knobs.

Recommended invariant wording recorded in
`crates/reddb-server/src/storage/cache/README.md`:

> Decision #218: this invariant applies to the homogeneous `sieve.rs::PageCache`
> workload. The Blob Cache L1 shard is the documented exception: because it
> protects user-visible cached blobs across L1/L2 tiers, `blob::Shard` may use
> `BlobCachePolicy::priority` as a bounded eviction bias before falling back to
> the `visited` sweep. Do not copy that second signal back into `PageCache`.

Evidence:

- `BlobCachePolicy::priority` is a first-class policy knob with default `128`
  and a builder/accessor in `crates/reddb-server/src/storage/cache/blob/mod.rs`.
- `Entry::new` stores the policy priority in `Entry.priority`, and `L2Record`
  persists priority in the L2 metadata format.
- `Shard::evict_one` checks `has_lower_priority_unvisited(entry.priority)`,
  so priority changes the selected victim after the normal visited-bit pass.
- The test `priority_biases_sieve_eviction_toward_lower_priority_entries`
  asserts the observable behavior: under pressure, a lower-priority entry is
  evicted while a higher-priority entry remains.
- ADR 0006 lists `priority` as "Bias memory admission / eviction" for Blob
  Cache policy and describes Blob Cache L1 as byte-bounded, sharded, and
  independent from the page cache.
