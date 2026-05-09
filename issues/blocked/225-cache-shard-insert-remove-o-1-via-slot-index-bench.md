# Cache: Shard insert/remove O(1) via slot-index + bench [BLOCKED]

GitHub: https://github.com/reddb-io/reddb/issues/225

Labels: enhancement, ready-for-agent, blocked

GitHub issue number: #225

## Status

Blocked. Kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#217

## What to build

`Shard::insert` (`blob.rs:1007`) e `remove` (`blob.rs:1080`) fazem `order.iter().position(|k| k == &key)` — O(n) varredura linear no `Vec<Key>` a cada operação. Substituir por slot-index estilo `sieve.rs` (invariante #5 do `cache/README.md`): cada entrada conhece sua posição em `slots`, lookup vira O(1).

Isto exige Shard isolado em `blob/shard.rs` (#224) primeiro.

Bench antes/depois usando o harness existente em `crates/reddb-server/benches/blob_cache_bench.rs` (#190): comparar throughput de insert/remove em shard com N=10k, N=100k items.

## Acceptance criteria

- [ ] `Shard` interno usa slot-index, sem `Vec<Key>` order linear.
- [ ] Eviction order observável idêntica ao SIEVE puro (testes de slice 7 continuam passando).
- [ ] Bench documenta delta de throughput em insert-heavy workload com N=10k e N=100k.
- [ ] Threshold mínimo: ≥10× speedup em N=100k insert/remove (caso contrário, reabrir investigação).
- [ ] Sem regressão em testes funcionais.

## Blocked by

- #224
