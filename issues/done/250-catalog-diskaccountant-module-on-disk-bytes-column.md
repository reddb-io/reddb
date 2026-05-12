# Catalog: DiskAccountant module + on_disk_bytes column [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/250

Labels: enhancement

GitHub issue number: #250

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Novo módulo `DiskAccountant` que calcula bytes em disco por collection, walking B-tree pages a partir de `collection_roots`. Cache TTL 30s por collection.

End-to-end:
- Módulo novo `storage/disk_accountant.rs` com interface `bytes_on_disk_for(collection: &str) -> u64`.
- Walking pages do B-tree partindo de `collection_roots[collection]`. Conta `pages × page_size`.
- Cache 30s (Mutex<HashMap<String, (u64, Instant)>>).
- Coluna `on_disk_bytes` em `red.collections` chama o accountant.
- Bench mede overhead: < 100ms por collection com 10k pages.
- Test empírico: insere N rows, valida que `on_disk_bytes` aumenta proporcional.

## Acceptance criteria

- [ ] `red.collections` retorna `on_disk_bytes` populado para cada Collection.
- [ ] Hot path (sem cache) < 100ms por Collection com 10k pages.
- [ ] Cache 30s reduz cost para sub-ms em queries repetidas.
- [ ] Test empírico: após inserir 10k rows × 1KB, `on_disk_bytes` cresce em pelo menos 95% × 10MB.
- [ ] Cobre tables relacionais e documents. Vector/timeseries: opcional nesta slice (sub-issue se complexo).
- [ ] Conformance: 1 case validando coluna presente e numérica.
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
