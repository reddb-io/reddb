RedDB Browser 0.6.0 adds a JSON-safe local database session bridge for Dart/Flutter Web and other JS-interop consumers. The Dart browser entrypoint now supports OPFS database opening, parameterized table SQL and BEGIN/COMMIT/ROLLBACK through the same Rust/WASM Worker used by JavaScript/TypeScript.

```sh
npm add @reddb-io/browser@0.6.0
```

`createInteropDatabaseSession()` exposes structured envelopes for ready, openDatabase, info, query, closeDatabase and close. Database tokens fence old handles after session reuse. The underlying session retains initialization, releases file ownership on database close and discards pending transactions. Native formats, SQL semantics and the Worker protocol are unchanged.

Dart consumers use `package:reddb/reddb_browser.dart`, `BrowserDatabaseSession.open(module)`, `session.openDatabase(name: ..., create: true)` and `db.query(sql, params: [...])`. The driver is available as repository source, not a pub.dev release; pin the Git revision and `drivers/dart` path.

Compiled Dart browser tests cover parameter binding, quotation/Unicode, durable commits, rollback, rollback on close, file-lock contention, session reuse, stale handles and complete offline Chromium/Firefox restarts. Real Safari/iOS persistence and Flutter's separate `--wasm` compilation mode remain unqualified. Table SQL is the supported local subset; other collection models, AI/events and synchronization remain unsupported.

Bundle/cache JS, Worker, WASM and the application locally. No RedDB server, account, API key or network service is required. Protect the entire open/read/change/commit/close operation with an application Web Lock across tabs. Retaining a session retains WASM memory; no CPU or memory reduction is claimed.

Usage and limits: https://github.com/reddb-io/reddb/blob/main/drivers/dart/BROWSER.md

The installable `.tgz` and `SHA256SUMS` accompany this release. Browser versioning is independent of native RedDB and publishes to npm's `latest` channel.
