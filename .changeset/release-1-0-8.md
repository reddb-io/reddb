---
"@reddb-io/cli": patch
"@reddb-io/sdk": patch
"@reddb-io/client": patch
"@reddb-io/client-bun": patch
---

Bundle of work that landed since `v1.0.7`.

**Query / engine**

- `SELECT … LIMIT $N OFFSET $N` parameterized — completes the `$N` bind sweep (`closes #361`).
- ASK responses now include inline `[^N]` citation markers, parsed by a dedicated `CitationParser` (`closes #393`).

**Drivers**

- **Go.** `c.Query(ctx, sql, params...)` accepts variadic `any` bind values, routed through the binary `QueryWithParams` frame when the server advertises `FEATURE_PARAMS`. Full Go → engine type mapping documented in the driver hub page (`closes #363`).
- **JS / TS.** RedWire `QueryWithParams` frame emit + `FEATURE_PARAMS` capability negotiation in the SDK and thin client.

**Infrastructure**

- `lru` bumped from `0.12` → `0.16`, picking up the upstream fix for the `IterMut` Stacked Borrows UB flagged by Dependabot (alerts #19, #20).
- CI workflows replace `arduino/setup-protoc@v3` (Node 20-deprecated) with a local `install-protoc` composite action that downloads protoc 28.3 directly. Eliminates the deprecation warning on every job.
- Dropped the unsupported `save-always: true` input from `Swatinem/rust-cache@v2` (silently ignored but emitted a warning per build).
