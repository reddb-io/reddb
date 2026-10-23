First experimental distribution of RedDB Browser for JavaScript/TypeScript applications.

```sh
npm add @reddb-io/browser@experimental
```

The package bundles the JavaScript API, TypeScript declarations, dedicated worker and Rust/WASM engine. Consumers need no Rust toolchain or RedDB server. Vite applications can import `@reddb-io/browser/worker?worker` and pass a fresh Worker to `openArtifact`.

This version exposes exclusive OPFS artifact persistence/recovery, RQL validation and storage-free scalar SELECT execution. Stored-data collection queries, application transactions, observation and optional synchronization remain under development. Artifact WAL/checkpoint methods operate on native binary payloads; they are not a CRUD/save-game API.

Offline loading uses the embedding application's cache or locally packaged assets. Chromium and Firefox have verified OPFS/offline restart coverage. Real Safari/iOS persistence remains unqualified.

Usage guide: https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md
API and engine details: https://github.com/reddb-io/reddb/tree/main/packages/browser

The `.tgz` below is also directly installable with npm/pnpm, and `SHA256SUMS` records its checksum. The Browser version/tag is independent of native RedDB releases; the npm channel is `experimental`.
