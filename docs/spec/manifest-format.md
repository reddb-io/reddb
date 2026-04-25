# Backup Manifest Format (v1.0)

Public, versioned spec for the backup catalog RedDB writes to a remote backend (S3-compatible, FS, HTTP). External tooling, third-party verifiers, and manual disaster-recovery scripts can rely on the layout below — incompatible schema changes will bump the major version.

## Layout under a backup prefix

```
<prefix>/
├── MANIFEST.json                                 # unified catalog (this spec)
├── snapshots/
│   ├── 000000000042-1730000000000.snapshot       # snapshot bytes
│   └── 000000000042-1730000000000.snapshot.manifest.json
└── wal/
    ├── 000000000042-000000000051.wal             # WAL segment bytes (JSON envelope)
    └── 000000000042-000000000051.wal.manifest.json
```

- **`MANIFEST.json`** — top-level catalog (this document). Lists every snapshot and WAL segment. Atomically updated via temp-then-rename.
- **`<key>.manifest.json`** — per-artifact sidecar. Holds `sha256` + size + LSN range. Restore reads these *first*; the unified catalog is for human / orchestrator inspection.

## `MANIFEST.json` schema

```json
{
  "version": "1.0",
  "engine_version": "0.1.5",
  "latest_lsn": 12500,
  "snapshots": [
    {
      "id": 42,
      "lsn": 12345,
      "ts": 1730000000000,
      "bytes": 1234567,
      "key": "snapshots/000000000042-1730000000000.snapshot",
      "checksum": "sha256:9f8b…"
    }
  ],
  "wal_segments": [
    {
      "lsn_start": 12345,
      "lsn_end": 12500,
      "key": "wal/000000012345-000000012500.wal",
      "bytes": 4096,
      "checksum": "sha256:c1d2…"
    }
  ]
}
```

### Fields

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `version` | string | yes | `"1.0"` for this revision. Major-version bump = incompatible schema. |
| `engine_version` | string | yes | `CARGO_PKG_VERSION` of the engine that wrote the manifest. Informational. |
| `latest_lsn` | uint64 | yes | Highest LSN known across all entries. `0` when only snapshots exist. |
| `snapshots[]` | array | yes | All snapshots under the prefix. Order is not guaranteed; readers should sort. |
| `snapshots[].id` | uint64 | yes | Engine-assigned snapshot id. |
| `snapshots[].lsn` | uint64 | yes | Base LSN the snapshot covers. |
| `snapshots[].ts` | uint64 | yes | Snapshot creation time, unix milliseconds. |
| `snapshots[].bytes` | uint64 | no | Size of the snapshot file. `0` when unknown. |
| `snapshots[].key` | string | yes | Backend key relative to the bucket / data root. |
| `snapshots[].checksum` | string | no | `"sha256:<hex>"`. Absent for legacy snapshots written before checksums were enforced. |
| `wal_segments[].lsn_start` | uint64 | yes | First LSN in the segment, inclusive. |
| `wal_segments[].lsn_end` | uint64 | yes | Last LSN in the segment, inclusive. |
| `wal_segments[].key` | string | yes | Backend key. |
| `wal_segments[].bytes` | uint64 | no | Encoded payload size. |
| `wal_segments[].checksum` | string | no | `"sha256:<hex>"`. Absent for legacy segments. |

## Per-artifact sidecar schema

`<artifact_key>.manifest.json`:

```json
{
  "key": "wal/000000012345-000000012500.wal",
  "lsn_start": 12345,
  "lsn_end": 12500,
  "size_bytes": 4096,
  "created_at": 1730000000000,
  "sha256": "c1d2…"
}
```

Snapshot sidecar (legacy name, used directly by restore):

```json
{
  "timeline_id": "main",
  "snapshot_key": "snapshots/000000000042-1730000000000.snapshot",
  "snapshot_id": 42,
  "snapshot_time": 1730000000000,
  "base_lsn": 12345,
  "schema_version": 1,
  "format_version": 1,
  "snapshot_sha256": "9f8b…"
}
```

## Restore semantics

1. Engine downloads the requested artifact (snapshot or WAL segment) to a local temp path.
2. Reads the sidecar manifest and extracts `sha256` (or `snapshot_sha256`).
3. Recomputes SHA-256 over the downloaded bytes.
4. **If the sidecar carries a digest and it does not match → restore fails closed.** The downloaded file is left in place for forensics.
5. **If the sidecar is absent or has no digest** (legacy archive, predates this spec) → restore proceeds with a `tracing::warn!` so operators know integrity coverage is degraded.
6. Snapshots open the database; WAL segments replay record-by-record, skipping any record at or below the snapshot's `base_lsn`.

## Atomicity

- Per-artifact sidecars are independent of each other; a partial publish never breaks unrelated artifacts.
- The unified `MANIFEST.json` is updated via temp-key + rename on filesystem backends. S3-compatible backends without conditional PUT use the fresh-temp-then-replace pattern; concurrent readers may briefly see the older or newer manifest, never a torn one. PUT-if-match support is a follow-up once the `RemoteBackend` trait grows conditional methods.

## Version history

- **1.0** — initial public release. Snapshots + WAL segments + per-artifact sidecars + unified catalog.
