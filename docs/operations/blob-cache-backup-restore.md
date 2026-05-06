# Blob Cache backup and restore — operator runbook

Pre-admin-handler playbook for managing **L2 Blob Cache** state across
backup, restore, and emergency invalidation. Pairs with:

- [ADR 0006 — tiered blob cache](../adr/0006-tiered-blob-cache.md) — L1/L2
  layout, membership synopsis, and recovery contract.
- [ADR 0009 — performance gate scope](../adr/0009-performance-gate-scope.md) —
  scope rules; cache backup is opt-in (deferred from #148).
- [`crates/reddb-server/src/storage/cache/sweeper.rs`](../../crates/reddb-server/src/storage/cache/sweeper.rs)
  — sweeper module landed by #148. The HTTP/CLI seams that will eventually
  drive it are still flagged in that file.
- [`docs/operations/blob-cache-dashboards.md`](blob-cache-dashboards.md) —
  in-flight via Lane #186; dashboards for the metrics this runbook references.
- [`docs/guides/cache-comparison.md`](../guides/cache-comparison.md) — when
  to reach for the Blob Cache vs. result cache vs. external Redis.

Several ergonomic seams (admin HTTP handler, `red admin blob-cache` CLI
subcommand, `REDDB_CACHE_BLOB_CLEAR_ON_START` env var) are not yet shipped.
Each gap is flagged inline so the runbook still tells you what to do today.

---

## 1. Current state of cache in backup (today)

**The Blob Cache is not part of a standard backup.** When you run

```bash
curl -X POST http://<host>:<port>/admin/backup \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN"
```

the snapshot covers the WAL chain, the unified collection store, and the
manifest catalog (per [`docs/spec/manifest-format.md`](../spec/manifest-format.md)).
**It does not include L1 entries, the L2 metadata B+ tree, or the L2 blob
chains.**

This is intentional. Per ADR 0006, the cache is by definition a *derived*
acceleration structure — its contents can always be rebuilt from the
authoritative L1 (the unified store) plus cold reads. Skipping it from
the standard catalog keeps backup cost bounded and stable as cache size
grows independently of the working set.

**Consequences for restore:** the cache starts empty, the membership
synopsis is empty, and the L2 metadata B+ tree opens against whatever
`cache.blob.l2_path` points at on the restored host (in a classic restore,
nothing). Latency on the first wave of post-restore reads will reflect
cold-path performance: every `get` misses both L1 and L2, falls through
to the primary store, and warms the cache as it goes.

**The opt-in is flagged but not yet wired.** Sweeper module flag #3 calls
out the planned `include_blob_cache: bool` knob on the backup orchestrator
(`runtime/backup.rs` follow-up). Until that knob lands, **there is no
supported way to include the cache in a standard backup**. The procedures
in §2 below are the manual workaround.

---

## 2. Manual L2 dump procedure

For operators who need to preserve cache state across a restart or
host-migration (warm cache after maintenance, big-bang region cutovers, etc.),
the L2 directory can be copied out of band.

### 2.1 Locate the L2 directory

The path is whatever the runtime resolved for `cache.blob.l2_path`. Find
it via `/admin/status`:

```bash
curl -s http://<host>:<port>/admin/status \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" | jq '.cache.blob.l2_path'
```

or, if the server is down, from the launch env / config you deployed with
(`BlobCacheConfig::with_l2_path`). Absent an explicit setting the runtime
runs L1-only and there is nothing to dump.

### 2.2 Server stopped — full directory copy

This is the safe path. Stop the server cleanly so the L2 metadata B+ tree
fsyncs on shutdown, then copy the entire directory:

```bash
systemctl stop reddb   # or: docker stop, kubectl delete pod, etc.
tar -C "$(dirname "$L2_PATH")" -czf "/backups/blob-cache-$(date -u +%Y%m%dT%H%M%SZ).tgz" "$(basename "$L2_PATH")"
```

The tarball is internally consistent because the server is stopped: no
in-flight writes, no half-committed metadata, generation counters are at
rest.

### 2.3 Server running — rsync diff procedure

Acceptable only when downtime is unavailable. Two-pass rsync with a
consistency window:

```bash
# Pass 1: bulk copy while the server runs.
rsync -aH --inplace "$L2_PATH/" "/backups/blob-cache-staging/"

# Pass 2: short window, immediately after pausing writers.
curl -X POST http://<host>:<port>/admin/readonly \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" -d '{"enabled": true}'
rsync -aH --inplace --delete "$L2_PATH/" "/backups/blob-cache-staging/"
curl -X POST http://<host>:<port>/admin/readonly \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" -d '{"enabled": false}'
```

**Two risks** of running rsync against a live server:

1. **In-flight writes.** A `BlobCache::put` racing with rsync may flush
   blob bytes to L2 pages but be killed (or not yet commit) before the
   metadata B+ tree update. The captured directory may then contain
   orphan chains. Not destructive on restore — the sweeper's
   `reclaim_orphans` reaps them on first call (see §6).
2. **Generation mismatch.** `invalidate_namespace` bumps a per-namespace
   generation counter. If rsync captures the metadata mid-bump, entries
   may carry a `namespace_generation` stale relative to the runtime's
   in-memory state. On restore the in-memory counters re-zero, so stale
   entries become visible again until they expire or get evicted —
   **a correctness regression for a flush operator** (data the operator
   told the cache to forget can come back). Do not rely on rsync captures
   across a flush.

Prefer the "stopped" path in §2.2.

---

## 3. Restore procedure

The L2 directory is written into place **before** server start. After that,
recovery is automatic.

### 3.1 Pre-start validation

Before bringing the server up against a restored L2 directory:

1. **Manifest checksum.** The L2 metadata B+ tree carries the engine's
   page-format checksum (per [`docs/spec/manifest-format.md`](../spec/manifest-format.md)
   and the WAL-ordering note in `docs/perf/blob-cache-l2-spike.md`). Use the
   shared verifier:

   ```bash
   red doctor verify-l2 --path "$L2_PATH" --json
   ```

   Exit 0 means the metadata file passes the page-checksum walk and the
   B+ tree opens cleanly. Anything else means **do not start with this
   directory** — fall back to a known-good copy or accept a cold cache by
   removing the directory entirely.

2. **Generation counter sanity.** When migrating between hosts, ensure no
   other writer has touched the destination L2 since the dump was taken
   (otherwise generation counters from two timelines will collide on
   first start). Easiest: restore into an empty `l2_path`.

3. **Permissions and ownership.** The runtime user must own `l2_path` and
   have read+write. The server will refuse to start if it cannot fsync.

### 3.2 Cold-start synopsis rebuild

Once the server starts and opens the L2 metadata B+ tree, the membership
synopsis is **rebuilt automatically** by `rebuild_l2_synopsis` (see
`crates/reddb-server/src/storage/cache/blob.rs` lines 1069–1085). The
function walks every record in the B+ tree and inserts `(namespace, key)`
into the per-namespace synopsis set.

**No operator action is required.** A few caveats:

- Rebuild cost is O(records) at startup. For very large L2 instances this
  can add measurable seconds to cold-start. Watch
  `reddb_blob_cache_l2_synopsis_rebuild_seconds` (per
  `docs/operations/blob-cache-dashboards.md`).
- A record that fails to decode is skipped with a warning; it does not
  fail the rebuild. Partial-corruption recovery is documented in §6.

### 3.3 Restore is complete when

- `red doctor` reports healthy.
- `reddb_blob_cache_l1_entries` is 0 (cold L1, expected — L1 warms on access).
- `reddb_blob_cache_l2_records` equals what was in the dump.

---

## 4. Sweeper invocation (when admin handler lands)

#148 deferred the HTTP handlers to a follow-up batch. **Today these
endpoints do not exist** — the sweeper module is bounded scaffolding only.
This section documents the contract so dashboards, smoke tests, and
operator muscle memory are ready when the handlers ship.

### 4.1 Planned: `POST /admin/blob_cache/sweep`

```json
{
  "limit": { "either": { "entries": 10000, "millis": 200 } }
}
```

Response shape (matching `SweepReport` in `sweeper.rs`):

```json
{
  "entries_scanned": 0,
  "entries_evicted": 0,
  "bytes_reclaimed": 0,
  "elapsed_ms": 0,
  "truncated_due_to_limit": false,
  "by_namespace": []
}
```

`limit` accepts the three `SweepLimit` variants: `{"entries": N}`,
`{"millis": N}`, or `{"either": {"entries": N, "millis": M}}`. When
`truncated_due_to_limit` is `true`, schedule another sweep; the report is
the operator's signal that there is more work waiting.

### 4.2 Planned: `POST /admin/blob_cache/flush_namespace`

```json
{ "namespace": "tenant-42:results" }
```

Returns `NamespaceFlushReport`:

```json
{
  "namespace": "tenant-42:results",
  "generation_before": 7,
  "generation_after": 8,
  "elapsed_micros": 38
}
```

Foreground-fast contract: `<100 µs` typical. If `elapsed_micros` regresses
into milliseconds, page on-call — generation bump should never block.

### 4.3 Status today: UNAVAILABLE

Both endpoints are flagged as #148 follow-up (see flag #4 at the top of
`sweeper.rs`). **Workaround until they ship:** restart the server. A
restart drops L1 entirely (it is process-local) and re-opens L2 from disk.
A `sweep_on_startup` runtime knob is also flagged (sweeper.rs flag #5)
but not wired. Until it is, restart-driven sweep is the only handle
operators have.

---

## 5. Namespace flush emergency procedure

This is the procedure for "we just realised tenant X's cache contains
data they should never have seen — purge it now." With the admin handler
unavailable the options narrow.

### 5.1 Direct call (requires custom CLI tool — NOT YET SHIPPED)

The fast path is `BlobCache::invalidate_namespace`, which bumps the
generation counter under a brief write-lock and returns immediately. There
is no in-tree binary that calls this method directly today.

**Spec for the planned tool** (proposed; not yet implemented):

```
red admin blob-cache flush-namespace <namespace> \
  [--bind <host>:<port>] \
  [--token "$RED_ADMIN_TOKEN"]
```

Output: the same `NamespaceFlushReport` JSON shown in §4.2. Implementation
should call the admin HTTP endpoint when available and fall back to a
direct in-process invocation for the embedded `red` deployment shape.
Tracking of this CLI subcommand is folded into the #148 follow-up batch.

### 5.2 Fallback today: server restart with cleared cache

Until the CLI tool ships, the only operator-driven namespace flush is a
**server restart that re-opens L2 from a freshly-cleared directory**:

```bash
systemctl stop reddb
rm -rf "$L2_PATH"           # destroys ALL namespaces, not just one
systemctl start reddb
```

This is heavy-handed: it flushes every namespace and forces a cold L2
re-warm against the primary. It is the only correctness-preserving option
when the targeted namespace contains data that **must not** be served
again under any circumstance.

A planned environment variable, `REDDB_CACHE_BLOB_CLEAR_ON_START=1`,
would make the restart-clear flow a one-line change without `rm -rf`.
**This env var is not yet implemented**; it is filed alongside the #148
follow-up so the full ergonomic story (`include_blob_cache`, admin
handler, CLI tool, clear-on-start env) lands as a coherent batch.

---

## 6. Disaster recovery

L2 corruption scenarios and how they recover.

### 6.1 Orphan blob chains after crash

**Symptom:** the process was killed (OOM, SIGKILL, host crash) after a
`put` flushed blob bytes to L2 pages but before the metadata B+ tree
commit. The pages are allocated but no record points at them.

**Recovery:** automatic on first sweep call.
`BlobCacheSweeper::reclaim_orphans` walks the L2 free-list, cross-checks
each chain root against the metadata, and reclaims any chain with no
metadata reference. Bounded by `SweepLimit`; if the report's
`truncated_due_to_limit` is `true`, schedule another invocation until it
is `false`. **Pre-admin-handler:** orphans accumulate silently until a
sweep runs. Today, the only thing that triggers a sweep is a future
runtime scheduler (sweeper.rs flag #5). Until then orphans sit on disk;
they are not data-loss, only wasted L2 capacity.

### 6.2 L2 directory deletion

**Symptom:** the entire `l2_path` is gone (operator `rm -rf`, volume
detach, disk wipe).

**Recovery:** the cache rebuilds **cold** from the primary L1 and cold
reads. No data loss, by definition — the cache is derived. Operators
lose the warm-restart benefit only.

If `cache.blob.l2_path` is still set in config, the runtime will recreate
the directory and metadata B+ tree on first put. If you want to disable
L2 entirely, unset the path in config and restart.

### 6.3 Partial L2 corruption

**Symptom:** the metadata B+ tree opens, but some records fail to decode
(bit-rot on a single page, partial overwrite).

**Recovery:** `rebuild_l2_synopsis` skips records whose `L2Record::decode`
fails, emitting a warning per skipped record. Healthy records remain
addressable; corrupt records are invisible (a `get` returns miss and
falls through to the primary).

To force full reconstruction: stop the server, move (don't delete)
`l2_path` aside for forensics, restart with an empty `l2_path`.

### 6.4 What is NOT a disaster

A cold cache. By design. If the only symptom you see is elevated p99
latency on the post-restore read wave, the cache is doing what it is
supposed to do — wait for it to warm.

---

## 7. Items flagged as not-yet-shipped

Tracked under #148 follow-up unless noted otherwise:

| Capability | Status | Workaround in this runbook |
|---|---|---|
| `include_blob_cache: bool` on `runtime/backup.rs` | **Not wired** | Manual L2 dump (§2). |
| `POST /admin/blob_cache/sweep` HTTP handler | **Not wired** | Restart server (§4.3). |
| `POST /admin/blob_cache/flush_namespace` HTTP handler | **Not wired** | `rm -rf l2_path` + restart (§5.2). |
| `red admin blob-cache flush-namespace` CLI | **Spec only** | Same as above. |
| `REDDB_CACHE_BLOB_CLEAR_ON_START=1` env var | **Proposed, not implemented** | Manual `rm -rf` before start. |
| `sweep_on_startup` runtime config | **Flagged in sweeper.rs** | Restart-driven sweep only. |
| `red doctor verify-l2` subcommand | **Proposed** | Trust-but-verify the dump path manually. |

When the orchestrator-batch lands, this runbook should be updated to
collapse §4.3 and §5.2 into thin pointers to the new endpoints, and §7
should shrink accordingly.
