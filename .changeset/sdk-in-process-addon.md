---
"@reddb-io/sdk": minor
---

Embedded mode can now run the engine in process. On platforms whose `@reddb-io/red-*` package ships `reddb.node`, `connect('memory://')` / `connect('file://…')` load it instead of spawning `red rpc --stdio` — same API and transaction semantics, no subprocess. Platforms without an addon, and calls that pass `options.binary`, keep the subprocess path. `REDDB_NATIVE_ADDON=/path/to/reddb.node` overrides the lookup. The addon aborts the process on an engine panic, like `red`.
