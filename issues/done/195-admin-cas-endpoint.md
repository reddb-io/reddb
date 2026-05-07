# Admin endpoint: POST /admin/cache/compare-and-set [AFK]

GitHub: reddb-io/reddb#195
Parent: #188

Sequential edit. Mirror Lane 5/5 sweep/flush handler patterns in handlers_admin.rs. Endpoint spec:

```
POST /admin/cache/compare-and-set
Body: { namespace, key, expected_version, new_value_b64, new_version, ttl_ms? }
```
- 200: { committed: true, current_version }
- 409: { committed: false, current_version, reason: "VersionMismatch" }
- 400: malformed body / CRLF/NUL injection / bad base64
- 401: missing/wrong admin token

Build BlobCachePut with version + ttl_ms; call BlobCache::put. Map CacheError::VersionMismatch → 409.

## Acceptance Criteria

- [ ] Handler in handlers_admin.rs + route registered in routing.rs.
- [ ] 10 tests: happy first-write, happy update, conflict, stale-expected, CRLF in namespace (400), NUL in key (400), missing/wrong bearer (401), bad base64 (400), concurrent CAS race (exactly 1 commits).
- [ ] HeaderEscapeGuard + SerializedJsonField at HTTP boundary.
- [ ] Mirror Lane 5/5 helper patterns 1:1 — no reinvention.
