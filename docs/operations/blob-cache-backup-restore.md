# Blob Cache backup and restore — operator runbook

Operator playbook for managing **L2 Blob Cache** state across backup,
restore, and emergency invalidation. The normal server uses the single-file
backend; the standalone pager procedures later in this document exist only for
direct engine callers using the compatibility backend. Pairs with:

- [ADR 0006 — tiered blob cache](../../.red/adr/0006-tiered-blob-cache.md) — L1/L2
  layout, membership synopsis, and recovery contract.
- [ADR 0009 — performance gate scope](../../.red/adr/0009-performance-gate-scope.md) —
  scope rules; cache backup is opt-in.
- [`crates/reddb-server/src/storage/cache/sweeper.rs`](../../crates/reddb-server/src/storage/cache/sweeper.rs)
  — sweeper module landed by #148, now driven by the live admin endpoints
  documented in §4.
- [`docs/operations/blob-cache-dashboards.md`](blob-cache-dashboards.md) —
  in-flight via Lane #186; dashboards for the metrics this runbook references.
- [`docs/guides/cache-comparison.md`](../guides/cache-comparison.md) — when
  to reach for the Blob Cache vs. result cache vs. external Redis.

The two `POST /admin/blob_cache/*` endpoints are live. The
`include_blob_cache=true` flag applies only to the legacy standalone L2 pager;
the server's internal L2 needs no separate archive step.

---

## 1. Server single-file backup and restore

The server stores L2 cache rows and their quota counter in the hidden
`red.cache.blob_l2` collection inside the primary `.rdb`. When you run

```bash
curl -X POST http://<host>:<port>/admin/backup \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN"
```

the snapshot covers that internal collection together with the WAL chain,
unified collection store and manifest catalog. L1 is process memory and is not
backed up. On restore, L1 starts cold and eligible values rehydrate from the L2
rows already present in the restored `.rdb`.

No cache sidecar, cache directory or `include_blob_cache` setting is required
for the normal server. Cache entries remain derived data: applications must be
able to recompute them, and operators may flush them after restore when a cold
start is preferred.

### 1.1 Legacy standalone backend: `include_blob_cache`

Direct engine callers can explicitly configure the older standalone pager via
`cache.blob.l2_path`. Only for that backend, preserve the warm L2 by setting:

```bash
curl -X POST http://<host>:<port>/config \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"red.config.backup.include_blob_cache": true}'
```

With the knob set, every `POST /admin/backup` (or the scheduled
backup-loop tick) additionally uploads the L2 pager file and its
control sidecar to the configured remote backend, under the
`{snapshot_prefix}blob_cache/` prefix (override via
`red.config.backup.blob_cache_prefix`). On restore, place those two
files back at `cache.blob.l2_path` (and `<l2_path>.blob-cache.ctl`)
**before** server start; the cold-start synopsis rebuild in
`BlobCache::new` (see `cache/blob/l2.rs::BlobCacheL2::rebuild_l2_synopsis`) re-indexes
the metadata automatically.

The L2 archive step is best-effort: per-file upload failures are logged
but never abort the snapshot+WAL backup, because the cache is derived
state. Watch `reddb_backup_failures_total` if you need an alert on
silent regressions.

The §2 manual procedure is useful only for a standalone L2 when the operator wants a
point-in-time L2 capture decoupled from the standard backup cadence
(host migrations, big-bang region cutovers).

---

## 2. Legacy standalone L2 dump procedure

The rest of this document applies only when `cache.blob.l2_path` was explicitly
configured by a direct engine caller. Do not use these sidecar procedures for
the normal server single-file backend.

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
`crates/reddb-server/src/storage/cache/blob/l2.rs`). The
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

## 4. Sweeper invocation (live admin endpoints)

Both endpoints are now live. They are gated by `RED_ADMIN_TOKEN` (when
set) like every other `/admin/*` route.

### 4.1 `POST /admin/blob_cache/sweep`

```bash
curl -X POST http://<host>:<port>/admin/blob_cache/sweep \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"limit_entries": 10000, "limit_millis": 200}'
```

Both fields are optional:

- Both omitted → unbounded sweep.
- One field set → `SweepLimit::Entries(N)` or `SweepLimit::Millis(N)`.
- Both set → first-bound-wins composite (`SweepLimit::Either`).

Response shape (matching `SweepReport` in `sweeper.rs`, plus `ok`):

```json
{
  "ok": true,
  "entries_scanned": 0,
  "entries_evicted": 0,
  "bytes_reclaimed": 0,
  "elapsed_ms": 0,
  "truncated_due_to_limit": false
}
```

When `truncated_due_to_limit` is `true`, schedule another sweep — the
report signals that there is more work waiting.

### 4.2 `POST /admin/blob_cache/flush_namespace`

```bash
curl -X POST http://<host>:<port>/admin/blob_cache/flush_namespace \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"namespace": "tenant-42:results"}'
```

Returns `NamespaceFlushReport` (plus `ok`):

```json
{
  "ok": true,
  "namespace": "tenant-42:results",
  "generation_before": 7,
  "generation_after": 8,
  "elapsed_micros": 38
}
```

Foreground-fast contract: `<100 µs` typical. If `elapsed_micros`
regresses into milliseconds, page on-call — generation bump should
never block.

`namespace` is required, must be non-empty, and must contain no
NUL/CR/LF bytes. The handler rejects adversarial inputs with `400` and
a structured error message before they reach the cache or the audit
log.

### 4.3 `GET /admin/blob_cache/stats`

Returns a JSON snapshot of global blob cache counters. No body required.

```bash
curl http://<host>:<port>/admin/blob_cache/stats \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN"
```

CLI equivalent:

```bash
red admin cache stats --bind <host>:<port>
red admin cache stats --bind <host>:<port> --pretty   # table format
```

Key fields: `hits`, `misses`, `entries`, `bytes_in_use`, `l2_bytes_in_use`,
`namespaces`, `l2_compression_ratio_observed`.

### 4.4 Background sweeper

A `sweep_on_startup` runtime knob is still flagged (sweeper.rs flag #5)
but not wired. Until it lands, scheduled sweeps are operator-driven via
§4.1 (e.g., a cron that POSTs every minute with a small per-call
budget).

---

## 5. Namespace flush emergency procedure

For "we just realised tenant X's cache contains data they should never
have seen — purge it now."

### 5.1 Fast path: admin endpoint

```bash
curl -X POST http://<host>:<port>/admin/blob_cache/flush_namespace \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" \
  -d '{"namespace": "tenant-42:results"}'
```

This bumps the per-namespace generation counter under a brief
write-lock; subsequent reads against the namespace miss until they
re-warm from the primary store. Foreground-fast (`<100 µs` typical) —
no service-impact window. See §4.2 for the full request/response
contract.

### 5.2 CLI wrapper

The `red admin cache` subcommand tree is shipped as of #198. All four
operations are available:

```bash
# Flush a namespace
red admin cache flush-namespace tenant-42:results --bind <host>:<port>

# Sweep expired entries
red admin cache sweep --bind <host>:<port>
red admin cache sweep --limit-entries 10000 --limit-millis 200 --bind <host>:<port>

# Read stats
red admin cache stats --bind <host>:<port>

# CAS update
red admin cache compare-and-set \
  --namespace ns --key k \
  --new-version 2 --value ./value.bin \
  [--expected-version 1] \
  --bind <host>:<port>
```

Set `RED_ADMIN_TOKEN` in the environment to pass the bearer token
automatically (mirrors `--token`). Set `REDDB_BIND_ADDR` to avoid
repeating `--bind`.

### 5.3 Last resort: server restart with cleared cache

For the rare case where the operator does **not** trust the
generation-bump path (e.g., suspected corruption in the in-memory
generation map), the heavy-handed fallback is a restart that re-opens
L2 from a freshly-cleared directory:

```bash
systemctl stop reddb
rm -rf "$L2_PATH"           # destroys ALL namespaces, not just one
systemctl start reddb
```

This flushes every namespace and forces a cold L2 re-warm against the
primary. Use only when the §5.1 path is unavailable or distrusted.

A planned environment variable, `REDDB_CACHE_BLOB_CLEAR_ON_START=1`,
would make the restart-clear flow a one-line change without `rm -rf`;
that knob remains unimplemented and is tracked separately.

---

## 6. Disaster recovery

L2 corruption scenarios and how they recover.

### 6.1 Orphan blob chains after crash

**Symptom:** the process was killed (OOM, SIGKILL, host crash) after a
`put` flushed blob bytes to L2 pages but before the metadata B+ tree
commit. The pages are allocated but no record points at them.

**Recovery:** call the admin sweep endpoint (§4.1).
`BlobCacheSweeper::reclaim_orphans` walks the L2 free-list, cross-checks
each chain root against the metadata, and reclaims any chain with no
metadata reference. Bounded by the `limit_entries` / `limit_millis`
fields; if the response's `truncated_due_to_limit` is `true`, POST
again until it is `false`. The orphans-reclaim path is wired into
`POST /admin/blob_cache/sweep` alongside the L1-expiry sweep.

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

## 7. Capability status

Live (no workaround needed):

| Capability | Where |
|---|---|
| `red.config.backup.include_blob_cache` config knob | §1.1, wired in `runtime/impl_core.rs::trigger_backup` |
| `POST /admin/blob_cache/sweep` HTTP handler | §4.1 |
| `POST /admin/blob_cache/flush_namespace` HTTP handler | §4.2, §5.1 |
| `GET /admin/blob_cache/stats` HTTP handler | §4.3 |
| `red admin cache` CLI subcommands | §5.2 |

Still pending:

| Capability | Status | Workaround in this runbook |
|---|---|---|
| `REDDB_CACHE_BLOB_CLEAR_ON_START=1` env var | **Proposed, not implemented** | Manual `rm -rf` before start (§5.3) |
| `sweep_on_startup` runtime config | **Flagged in sweeper.rs** | Operator-driven sweep cron (§4.4) |
| `red doctor verify-l2` subcommand | **Proposed** | Trust-but-verify the dump path manually |
