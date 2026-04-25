# RedDB WAL Format (v2)

Public, versioned spec for the Write-Ahead Log surfaces RedDB exposes — the on-disk **logical WAL spool** the primary writes to before acking, and the **archived WAL segment** format published to remote backends for replication and PITR. Pairs with [`manifest-format.md`](manifest-format.md) (catalog) and [`admin-api.openapi.yaml`](admin-api.openapi.yaml) (operator API).

External tooling, third-party verifiers, and disaster-recovery scripts can rely on the layouts below — incompatible schema changes bump the major version.

## Surfaces

| Surface | Where | Purpose |
|---------|-------|---------|
| Logical WAL spool | `<data_path>/.logical-wal/` | Local durability for replication; primary appends here before acking commits. |
| Archived segment | `<wal_prefix><lsn_start>-<lsn_end>.wal` (remote backend) | Compact, externally verifiable snapshot of a contiguous LSN range. |
| Per-segment sidecar | `<segment_key>.manifest.json` (remote) | Integrity metadata: sha256, lsn range, prev_hash. |

## 1. Logical WAL Spool (on-disk, v2)

The primary's local spool. Each ack'd commit appends one or more **logical change records** to a sequence of fixed-name files, fsynced before the commit returns.

### Frame format

Every record is a self-describing frame:

```
[magic       4 bytes = "RDLW"]
[version     1 byte  = 0x02]
[lsn         8 bytes little-endian u64]
[timestamp   8 bytes little-endian u64 (unix ms)]
[payload_len 4 bytes little-endian u32]
[payload     payload_len bytes]
[crc32       4 bytes little-endian u32]
```

- **Magic** (`RDLW` = "RedDB Logical WAL"): identifies a valid frame start. A reader scanning for the next valid record looks for this exact byte sequence.
- **Version**: currently `0x02`. Old `v1` frames (different layout) are read-only legacy; the engine never writes v1.
- **LSN**: monotonically increasing across the spool. Replay enforces strict `+1` adjacency on the apply path (PLAN.md Phase 11.5).
- **Timestamp**: UNIX milliseconds at append time. Used by PITR for `target_time` filtering.
- **Payload**: opaque to the spool — defined by the [Logical Change Record](#11-logical-change-record-payload) section below.
- **CRC32**: computed over `version || lsn || timestamp || payload_len || payload`. Magic is *excluded* from the digest so a torn append corrupts the magic but leaves the CRC bytes recognizable to the recovery scanner.

### Recovery semantics

On open, the spool reader walks forward from offset 0:

1. **Valid frame** — magic + CRC match → record returned to the apply loop.
2. **Magic mismatch** — corrupt/truncated tail. Reader stops, returns the prefix that was valid. Truncate-on-write semantics are documented; the spool's max valid offset is exposed to the writer so the next append overwrites the bad tail.
3. **CRC mismatch** with valid magic — corrupted record body, fail closed. The operator is expected to investigate before resuming.

The spool is **append-only at the application level**; physical truncation only happens after a record has been archived (see §2) and pruned via `prune_through(lsn)`.

### 1.1 Logical Change Record payload

Within a spool frame, the payload is a `ChangeRecord` (see `src/replication/cdc.rs`):

```
[lsn          8 bytes le u64]      // duplicate of frame.lsn for self-describing payload
[timestamp    8 bytes le u64]      // duplicate of frame.timestamp
[operation    1 byte]              // 0=Insert, 1=Update, 2=Delete
[collection_len  4 bytes le u32]
[collection      collection_len bytes UTF-8]
[entity_id    8 bytes le u64]
[entity_kind_len 4 bytes le u32]
[entity_kind     entity_kind_len bytes UTF-8]
[entity_bytes_present 1 byte]      // 0 = absent, 1 = present
[entity_bytes_len 4 bytes le u32]  // only when present == 1
[entity_bytes    entity_bytes_len bytes]  // serialized UnifiedEntity
[metadata_present 1 byte]
[metadata_len    4 bytes le u32]   // only when present == 1
[metadata        metadata_len bytes UTF-8 JSON]
```

The `entity_bytes` blob is encoded by `UnifiedStore::serialize_entity` and is parameterized by `REDDB_FORMAT_VERSION` — readers that don't recognise the engine version SHOULD refuse to apply.

## 2. Archived WAL Segment (remote backend)

When the primary archives a contiguous range of records, it publishes:

- A **payload object** at `<wal_prefix><lsn_start:012>-<lsn_end:012>.wal`.
- A **sidecar manifest** at `<segment_key>.manifest.json` (see §3).

### Payload

The payload is a JSON array of records:

```json
[
  {"lsn": 1, "data": "<hex>"},
  {"lsn": 2, "data": "<hex>"}
]
```

- `lsn` (uint64) — must match the LSN of the embedded change record.
- `data` (hex string) — hex-encoded bytes of the [Logical Change Record payload](#11-logical-change-record-payload). NOT the full spool frame — the magic/version/crc are spool-only.

The JSON envelope is intentionally simple so external tools (audit scripts, replicator services) can parse without proto schemas. A future v2 binary segment format may land alongside; readers MUST sniff the sidecar's `version` field (when present) before parsing.

### Range encoding

The key is `<wal_prefix><lsn_start>-<lsn_end>.wal` with both numbers zero-padded to 12 digits. This keeps lexicographic listing in LSN order on every backend.

## 3. Per-Segment Sidecar Manifest

`<segment_key>.manifest.json`:

```json
{
  "key": "wal/000000000010-000000000019.wal",
  "lsn_start": 10,
  "lsn_end": 19,
  "size_bytes": 4096,
  "created_at": 1730000000000,
  "sha256": "9f8b…",
  "prev_hash": "c1d2…"
}
```

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `key` | string | yes | Backend key of the payload; redundant with the sidecar key but lets a reader handle the sidecar in isolation. |
| `lsn_start` / `lsn_end` | uint64 | yes | Inclusive LSN range. |
| `size_bytes` | uint64 | no | Encoded payload size. |
| `created_at` | uint64 | no | Unix milliseconds when archived. |
| `sha256` | string | no | Hex SHA-256 of the payload bytes. Restore re-hashes after download and fails closed on mismatch. |
| `prev_hash` | string | no | Hex SHA-256 of the immediately prior segment's `sha256`. Restore validates `segment[i].prev_hash == segment[i-1].sha256`. `null` only for the first segment of a fresh timeline. |

## 4. Hash Chain (PLAN.md Phase 11.3)

The `prev_hash` field forms a forward-only chain across all archived segments in a timeline. Restore enforces it strictly:

1. The first segment in the restore plan (lowest `lsn_start`) may have `prev_hash = null`.
2. Every subsequent segment **must** declare `prev_hash` matching the prior segment's `sha256`. Any of these breaks the chain and aborts restore:
   - Missing middle segment — next segment's `prev_hash` refers to a sha that wasn't loaded.
   - Tampered or replaced segment — `prev_hash` does not match what was loaded.
   - Reorder — a segment claiming `prev_hash = null` appears after an earlier one.
3. Legacy archives without a sidecar at all skip the chain check with `tracing::warn!`. Once the engine writes a v2 sidecar, the chain becomes mandatory from that point forward.

The runtime persists the active chain head at `red.config.timeline.last_segment_hash` in the database's `red_config` store. Each successful `archive_change_records` call advances this value to the new segment's `sha256`.

## 5. Versioning Policy

| Surface | Version field | Compat policy |
|---------|---------------|---------------|
| Spool frame | `version` byte | v1 read-only legacy; v2 current; bumps require dual-read window of one release. |
| Archived payload | (no inline version yet) | Sniff the sidecar's `version`; future binary v2 envelopes will carry magic in the body. |
| Sidecar manifest | (no field yet — v1 implied) | Additive fields only inside v1.x. New fields must be optional. |
| Hash chain semantics | This document | Stricter checks (e.g. mandatory chain) bump this doc's major. |

## 6. Out of Scope

- Compression: payloads are uncompressed today. Future evolution will pick zstd via a `codec` field in the sidecar (PLAN.md Phase 9.4).
- Encryption-at-rest of WAL segments: the engine's pager-level encryption (Phase 6.3 foundation) does not yet apply to archived segments. Operators using object-store-side encryption (S3-SSE, R2 KMS) cover the at-rest concern today.
- Multipart resumable uploads: archived segments are uploaded as a single PUT. Resume on partial failure is a Phase 8.3 follow-up.

## Version History

- **v2 (current)** — RDLW frame v2 with timestamp + CRC32; hash chain in sidecar; spec public.
- v1 — initial framed format without hash chain. Read-only legacy.
