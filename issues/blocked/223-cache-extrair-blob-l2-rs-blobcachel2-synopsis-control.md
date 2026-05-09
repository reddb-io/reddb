# Cache: extrair blob/l2.rs (BlobCacheL2 + synopsis + control) [BLOCKED]

GitHub: https://github.com/reddb-io/reddb/issues/223

Labels: enhancement, ready-for-agent, blocked

GitHub issue number: #223

## Status

Blocked. Kept out of Ralph's top-level queue.

## Original GitHub Body

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

- [ ] `blob/l2.rs` contém `BlobCacheL2` e tipos correlatos (control, synopsis).
- [ ] `blob/mod.rs` permanece sem o código L2.
- [ ] API exportada em `cache/mod.rs` inalterada.
- [ ] Testes de L2 round-trip + synopsis rebuild migrados para `blob/l2.rs::tests`.
- [ ] `cargo test -p reddb-server` passa, incluindo `cache/mod.rs::backup_helpers_tests::full_round_trip_via_blob_cache_preserves_entries_after_restore`.
- [ ] `wc -l blob/l2.rs` ≤ 2000.

## Blocked by

- #222
