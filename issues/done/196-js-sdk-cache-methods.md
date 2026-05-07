# JS/TS SDK: cache.* methods in @reddb-io/sdk + @reddb-io/client [AFK]

GitHub: reddb-io/reddb#196
Parent: #188

Additive parallel — only touch drivers/js/src/ + drivers/js-client/src/ + tests. NO Rust files.

For each of drivers/js/ and drivers/js-client/:

1. New `src/cache.js` — implements `client.cache.{get,put,exists,invalidate,invalidatePrefix,invalidateTags,flushNamespace}`.
2. New `src/cache-types.d.ts` (or extend index.d.ts) — TypeScript types.
3. New `test/cache.test.mjs` — round-trip + exists + invalidate variants + mock-server tests.

If gRPC RPCs absent in proto (verify), FLAG and ship via admin HTTP endpoints (sweep/flush already exist post-Lane 5/5).

## Acceptance Criteria

- [ ] Both packages export same typed cache API.
- [ ] Round-trip integration test against local server.
- [ ] Mock-server tests for offline CI.
- [ ] EmbeddedNotSupported handling consistent with other client APIs.
- [ ] TS types match Rust BlobCachePolicy fields semantically.
- [ ] Both `pnpm test` suites pass.
