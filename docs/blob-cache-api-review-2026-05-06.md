# Blob Cache — Public API Review (2026-05-06)

- **Status:** Review draft (HITL)
- **Reviewer:** claude-opus-4-7 (per session)
- **Tracking issue:** [#151](https://github.com/reddb-io/reddb/issues/151)
- **Subject:** `crates/reddb-server/src/storage/cache/blob.rs` (re-exports in `cache/mod.rs`), with sweeper integration via `cache/sweeper.rs`
- **Reference docs:** PRD [#139](https://github.com/reddb-io/reddb/issues/139), ADR 0006 (tiered blob cache), ADR 0010 (serialization-boundary discipline), `docs/guides/cache-comparison.md`

Evaluates whether the current public surface is the right shape to commit to before downstream callers (#143 result-cache adapter, #146 Bloom synopsis, future cluster invalidation) start depending on it. Read-only audit.

## Surface inventory

| Item | Kind | Re-exported | Doc |
|:---|:---|:---:|:---:|
| `BlobCache` | struct | yes | module-level only |
| `BlobCache::new(BlobCacheConfig)` | ctor | yes | none |
| `BlobCache::with_defaults()` | ctor | yes | none |
| `BlobCache::put(ns, key, BlobCachePut)` | method | yes | none |
| `BlobCache::get(ns, key)` | method | yes | none |
| `BlobCache::exists(ns, key)` | method | yes | none |
| `BlobCache::invalidate_key(ns, key)` | method | yes | yes |
| `BlobCache::invalidate_prefix(ns, prefix)` | method | yes | yes (1 line) |
| `BlobCache::invalidate_tag(ns, tag)` | method | yes | yes (1 line) |
| `BlobCache::invalidate_dependency(ns, dep)` | method | yes | yes (1 line) |
| `BlobCache::invalidate_namespace(ns)` | method | yes | yes |
| `BlobCache::stats()` | method | yes | none |
| `BlobCache::config()` | method | yes | none |
| `BlobCacheConfig` + 7 builder fns | struct | yes | none |
| `BlobCachePut` + 4 builder fns | struct | yes | none |
| `BlobCachePolicy` + 6 builder fns | struct | yes | none |
| `BlobCacheHit { bytes, content_metadata, version }` | struct (fields pub) | yes | none |
| `BlobCacheStats` (19 pub fields) | struct (fields pub) | yes | none |
| `L1Admission { Always, Auto, Never }` | enum | yes | none |
| `CacheError` (6 variants) | enum | yes | none |
| `DEFAULT_BLOB_*` (7 consts) | const | partial (4/7) | none |
| `METRIC_CACHE_*` (4 consts) | const | yes | none |
| `BlobCacheSweeper::{sweep_expired, reclaim_orphans, flush_namespace}` | fn | via `sweeper` mod | yes |
| `SweepLimit`, `SweepReport`, `OrphanReport`, `NamespaceFlushReport`, `NamespaceSweepStats` | struct/enum | via `sweeper` mod | partial |

## Findings by severity

### Critical

None. The shape is broadly aligned with ADR 0006 and absorbs the sweeper cleanly.

### High

1. **PRD vocabulary drift on `invalidate_tag` / `invalidate_dependency`.** ADR 0006 §Interface specifies plural batched forms (`invalidate_tags`, `invalidate_dependencies`); current methods take exactly one tag/dep. Once #143 starts emitting batched table-write invalidations, N singular calls multiply lock acquisitions. Add `invalidate_tags(ns, impl IntoIterator<Item=&str>)` and `invalidate_dependencies(ns, impl IntoIterator<Item=&str>)` before #143 lands.

2. **`CachePresence` from ADR 0006 is missing.** `exists()` returns `bool`, collapsing `Present` / `Absent` / `MaybePresent`. Forward-compat trap: when #146 ships and the synopsis answer can differ from metadata, callers with `if cache.exists(...)` silently shift semantics. Introduce `CachePresence` and add `presence(ns, key) -> CachePresence` (or `presence_fast` keeping `exists` as the strict variant). Decide before #146.

3. **`pub` fields on `BlobCacheStats`, `BlobCacheHit`, `BlobCacheConfig`, `BlobCachePolicy`.** Any future field rename or removal is a breaking change. #146 will need `synopsis_hits` / `synopsis_false_positives`; the policy is missing PRD stories #8–#10 fields. Make fields `pub(crate)` with accessors, or annotate `#[non_exhaustive]` so adding fields is non-breaking.

4. **`Send` / `Sync` contract is undocumented.** `BlobCache` is implicitly `Send + Sync` (parking_lot, atomics, `Arc<[u8]>`) but no doc comment commits to it. The runtime scheduler and admin handlers will share `Arc<BlobCache>` across threads. Add a `# Concurrency` rustdoc section plus a compile-time `assert_send_sync::<BlobCache>()`.

### Medium

5. **Typed-guard discipline (ADR 0010) not applied to keys/namespaces.** `namespace`, `key`, `tag`, `dependency`, `prefix` are raw `&str`. ADR 0010 explicitly leaves "anything else that a caller can shape and a downstream parser can be tricked by" deferred. Cache keys reach metrics labels, L2 file paths, and audit-style observability — the same surfaces ADR 0010 hardens. Add a thin `Namespace` newtype validating printable-ASCII / no-control-chars; keys can stay byte-arbitrary but should never reach log macros without going through `Tainted<&str>`. Follow-up; not blocking this slice.

6. **`CacheError::L2Io(String)` is opaque.** Stringified errors lose `io::ErrorKind`, defeat caller `match`, and hide whether the cause is `NoSpace` / `PermissionDenied` / `BrokenPipe`. Move to `L2Io { kind: io::ErrorKind, context: &'static str }` or `thiserror` `#[source]`. PRD story #34 ("admin operations for sweep") implies actionable taxonomy.

7. **Sweeper accessors must stay crate-private.** `sweeper.rs` flags placeholder `(0, 0)` generation values pending a `BlobCache::current_generation(&str) -> u64` accessor (plus `for_each_l1_entry`, `for_each_l2_record`, `l2_orphan_chains`). These are iteration-semantics leaks and must land as `pub(super)` or `pub(crate)`, never `pub`.

8. **`BlobCachePut::with_content_metadata` clobbers prior calls.** Consistent with `with_tags` / `with_dependencies`, but flag in rustdoc; not a code change.

### Low

9. **Naming.** PRD verbs (`put`, `get`, `exists`, `invalidate_*`) match. ADR 0006 §Interface uses `cache_get` / `cache_exists` for an eventual SQL/HTTP surface. No change needed; flag for the #151 public-API decision.

10. **Inconsistent re-exports.** `cache/mod.rs` re-exports 4 of 7 `DEFAULT_BLOB_*` constants — `DEFAULT_BLOB_SHARDS`, `DEFAULT_CONTENT_METADATA_KEYS_MAX`, `DEFAULT_CONTENT_METADATA_BYTES_MAX` are missing. Pick one: re-export all or none.

11. **Missing PRD policy fields.** `BlobCachePolicy` has no `idle_ttl_ms`, `stale_ttl_ms`, or `jitter_pct` (ADR 0006 §"Cache policy"; PRD stories #8–#10). Add as `Option<...>` now or apply `#[non_exhaustive]` (covered by Finding 3).

12. **No `put` receipt.** ADR 0006 specifies `CacheWriteReceipt`. Today `put` returns `Result<(), CacheError>`. A receipt with `{ admitted_to_l1, admitted_to_l2, evicted_bytes }` would expose admission decisions without `stats()` polling. Low priority — `()` is a reasonable v0.

13. **Blocking semantics undocumented.** All methods are sync; `put` may perform L2 disk I/O on the calling thread. Tokio callers must use `spawn_blocking`. Add a `# Blocking` rustdoc paragraph; `BlobCacheAsync` wrapper out of scope.

## Forward-compat check against open follow-ups

| Follow-up | Compatible as-is? | Notes |
|:---|:---:|:---|
| #143 result-cache adapter (batched dependency invalidation) | partial | requires plural `invalidate_dependencies` (Finding 1) |
| #146 Bloom synopsis upgrade | no | requires `CachePresence` enum (Finding 2) and new stats fields (Finding 3) |
| Cluster-wide invalidation (ADR 0008 future) | yes | `invalidate_*` methods are documented as node-local; cluster propagation can layer on top |
| Public HTTP/SQL surface (#151 decision pending) | partial | typed namespace/key guard (Finding 5) and richer error taxonomy (Finding 6) needed before any external surface |
| Idle TTL / stale serving / jitter (PRD stories #8–#10) | no | requires `BlobCachePolicy` fields (Finding 11) |

## Recommendation

**Ship with 4 changes.**

Block the result-cache adapter (#143) and Bloom synopsis (#146) on:

1. Plural `invalidate_tags` / `invalidate_dependencies` (Finding 1).
2. `CachePresence` enum and a presence-returning method (Finding 2).
3. `#[non_exhaustive]` on `BlobCacheStats`, `BlobCacheHit`, `BlobCacheConfig`, and `BlobCachePolicy`, or convert pub fields to accessors (Findings 3, 11).
4. Documented `Send + Sync` contract plus blocking-semantics paragraph on `BlobCache` (Findings 4, 13).

Findings 5 (typed-guard taint), 6 (`CacheError::L2Io` taxonomy), 7 (sweeper accessor visibility), 12 (put receipt) are follow-ups, not blockers for the internal-only surface. Public HTTP/SQL exposure stays deferred until those land.

The shape is good. The four blocking items are forward-compat hygiene, not redesign — fixing them now is much cheaper than deprecating field-pub structs after the result-cache adapter starts shipping.
