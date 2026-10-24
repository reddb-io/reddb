RedDB Browser 0.1.0-alpha.1 adds a versioned shared Rust/WASM runtime for JavaScript/TypeScript and language interop.

```sh
npm add @reddb-io/browser@experimental
```

The package bundles the JavaScript API, TypeScript declarations, dedicated worker and Rust/WASM engine. Consumers need no Rust toolchain or RedDB server. Vite applications can import `@reddb-io/browser/worker?worker` and pass a fresh Worker to `openArtifact`.

New in this version:

- `createRuntime()` validates RQL and executes storage-free scalar SELECTs without OPFS or Web Locks.
- `info()` reports ABI version, exact-value encoding and supported capabilities. SDK/Worker/WASM mismatches fail before opening storage.
- `createInteropRuntime()` provides JSON-safe success/error envelopes and Worker cancellation for JS interop hosts.
- Source adapters and real browser probes cover Dart/Flutter Web and C#/Blazor. They are not published to pub.dev/NuGet.

This version exposes exclusive OPFS artifact persistence/recovery, RQL validation and storage-free scalar SELECT execution. Stored-data collection queries, application transactions, observation and optional synchronization remain under development. Artifact WAL/checkpoint methods operate on native binary payloads; they are not a CRUD/save-game API.

Offline loading uses the embedding application's cache or locally packaged assets. Chromium and Firefox have verified OPFS/offline restart coverage. Real Safari/iOS persistence remains unqualified.

Usage guide: https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md
API and engine details: https://github.com/reddb-io/reddb/tree/main/packages/browser

The `.tgz` below is also directly installable with npm/pnpm, and `SHA256SUMS` records its checksum. The Browser version/tag is independent of native RedDB releases; the npm channel is `experimental`.
