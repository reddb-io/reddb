# Cache: extrair blob/l2.rs (BlobCacheL2 + synopsis + control) [AFK]

## Parent

#217

## What to build

Segunda fatia do split de `blob.rs`. Extrair `BlobCacheL2` (linhas ~1508-2095) para `blob/l2.rs`.

Inclui:
- `BlobCacheL2::open` e seus 7 `AtomicU64` próprios.
- Pager file + metadata B+ tree.
- Synopsis (Bloom filter) + `rebuild_l2_synopsis`.
- Control sidecar handling.
- Métricas L2 (`METRIC_CACHE_BLOB_L2_*`).

Re-exports de `blob/mod.rs` preservam API. Sem mudança de comportamento — apenas movimento físico.

## Acceptance criteria

- [x] `blob/l2.rs` contém `BlobCacheL2` e tipos correlatos (control, synopsis).
- [x] `blob/mod.rs` permanece sem o código L2.
- [x] API exportada em `cache/mod.rs` inalterada.
- [x] Testes de L2 round-trip + synopsis rebuild migrados para `blob/l2.rs::tests`.
- [ ] `cargo test -p reddb-server` passa, incluindo `cache/mod.rs::backup_helpers_tests::full_round_trip_via_blob_cache_preserves_entries_after_restore`.
- [x] `wc -l blob/l2.rs` ≤ 2000.

## Blocked by

- #222

## Done Notes

- Extracted `BlobCacheL2`, Bloom synopsis, rebuild logic, and L2 blob-chain/control handling to `crates/reddb-server/src/storage/cache/blob/l2.rs`.
- Kept `BlobCache` API and cache-level public re-exports unchanged.
- Migrated focused L2 round-trip, compression round-trip, Bloom sizing/FPR, and synopsis rebuild tests into `blob/l2.rs::tests`.
- Verification:
  - `cargo check -p reddb-server --lib --quiet`
  - `CARGO_TARGET_DIR=target/agent-223 cargo test -p reddb-server storage::cache::blob::l2::tests --quiet`
  - `CARGO_TARGET_DIR=target/agent-223 cargo test -p reddb-server full_round_trip_via_blob_cache_preserves_entries_after_restore --quiet`
  - `CARGO_TARGET_DIR=target/agent-223 cargo test -p reddb-server storage::cache::blob --quiet`
  - `CARGO_TARGET_DIR=target/agent-223 cargo test -p reddb-server --quiet` ran; cache tests passed, but the full suite has 6 unrelated pre-existing/non-cache failures listed in the worker final note.
