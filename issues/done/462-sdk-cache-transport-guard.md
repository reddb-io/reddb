# SDK Cache transport guard: throw `UNSUPPORTED_TRANSPORT` on embedded [AFK]

Labels: bug, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

The SDK exposes `db.cache.{get, put, exists, invalidate, invalidatePrefix, invalidateTags, flushNamespace}` regardless of transport, but the underlying `cache.*` RPC methods are only served by the HTTP path (the source comment in `cache.js` calls this out: "endpoints planned for a future server release"). On embedded (`file://` / stdio JSON-RPC), every call surfaces as `RedDBError: unknown method: cache.put` from deep inside the call chain.

Add a transport guard: when the underlying transport does not implement `cache.*`, every `CacheClient` method throws `UNSUPPORTED_TRANSPORT` with a clear message before issuing the RPC call. The error names the method and the transport so users can adjust without reading the source.

Update the typed surface (`index.d.ts`) and the doc comment so the limitation is discoverable from autocomplete.

## Acceptance criteria

- [ ] Every `CacheClient` method throws `UNSUPPORTED_TRANSPORT` on stdio / embedded transports before any RPC call is issued.
- [ ] Error message names the offending method and the transport.
- [ ] HTTP / gRPC transports continue to call through unchanged.
- [ ] `index.d.ts` has a JSDoc note that the client requires HTTP / gRPC.
- [ ] Tests cover: embedded throws on each method; HTTP mock continues to call through.

## Blocked by

None - can start immediately.
