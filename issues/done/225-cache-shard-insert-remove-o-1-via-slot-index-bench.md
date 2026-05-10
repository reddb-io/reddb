# Cache: Shard insert/remove O(1) via slot-index + bench [AFK]

## Parent

#217

## What to build

`Shard::insert` (`blob.rs:1007`) e `remove` (`blob.rs:1080`) fazem `order.iter().position(|k| k == &key)` — O(n) varredura linear no `Vec<Key>` a cada operação. Substituir por slot-index estilo `sieve.rs` (invariante #5 do `cache/README.md`): cada entrada conhece sua posição em `slots`, lookup vira O(1).

Isto exige Shard isolado em `blob/shard.rs` (#224) primeiro.

Bench antes/depois usando o harness existente em `crates/reddb-server/benches/blob_cache_bench.rs` (#190): comparar throughput de insert/remove em shard com N=10k, N=100k items.

## Acceptance criteria

- [x] `Shard` interno usa slot-index, sem `Vec<Key>` order linear.
- [x] Eviction order observável idêntica ao SIEVE puro (testes de slice 7 continuam passando).
- [x] Bench documenta delta de throughput em insert-heavy workload com N=10k e N=100k.
- [x] Threshold mínimo: ≥10× speedup em N=100k insert/remove (caso contrário, reabrir investigação).
- [x] Sem regressão em testes funcionais.

## Completion note

Implemented in this slice:

- `blob/shard.rs::Shard` now uses stable `slots` plus a free-list, and each
  in-memory `Entry` carries its `slot_index`.
- Replacing an existing key updates the same slot; removing/evicting an entry
  empties the slot without shifting surviving keys.
- Added shard unit tests for replacement/removal slot invariants.
- Added `w9-shard-insert-remove-slot-index` to
  `crates/reddb-server/benches/blob_cache_bench.rs`.
- Recorded local bench results in
  `docs/perf/blob-cache-shard-slot-index-2026-05-10.md`.

Verification:

- `cargo test -p reddb-server storage::cache::blob -- --nocapture` passed.
- `cargo check -p reddb-server --benches` passed.
- `cargo bench -p reddb-server --bench blob_cache_bench w9-shard-insert-remove-slot-index`
  passed; N=100k current path measured 106.67 ms for put+invalidate, and
  standalone pre/post algorithm check measured 122.9x speedup at N=100k.
- `make check` passed.
- `cargo test -p reddb-server` still fails in unrelated admin CAS,
  coercion_spine, and telemetry admin intent log tests.
- `pnpm test` still fails because `@reddb-io/internal-bin-resolver` is missing.
- `pnpm typecheck` still fails because the command is not defined.

## Blocked by

- #224
