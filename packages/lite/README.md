# RedDB Lite — experimental persistence foundation

This private package is the first building block of RedDB Lite. It runs the
canonical Rust `.rdb` artifact publication/recovery code and RQL parser in a
dedicated WebAssembly worker, persisting through a real OPFS sync access handle.
It requires a secure context, module workers, OPFS and Web Locks.

**It is not a database SDK yet.** It does not execute SQL, expose the five data
models, synchronize with a server, resolve conflicts, or forward requests between
tabs. It must not be published or substituted for `@reddb-io/client`.

## Build and verify

Install the repository-pinned Rust toolchain, its `wasm32-unknown-unknown` target,
and `wasm-bindgen-cli` **0.2.120**. Install workspace dependencies with pnpm.

```sh
pnpm --filter @reddb-io/lite build
pnpm --filter @reddb-io/lite exec playwright install --with-deps chromium firefox webkit
pnpm --filter @reddb-io/lite test
cargo test --locked -p reddb-io-file
```

The `WASM_BINDGEN` environment variable can point at the pinned CLI executable.
`REDDB_LITE_BROWSERS=chromium` restricts the local browser test matrix; CI runs all
three engines. Generated JS/WASM bindings are ignored and rebuilt from Rust.

The tests probe the browser's actual native APIs. If a port lacks synchronous
OPFS (currently Playwright's WPE WebKit runner), its persistence tests explicitly
report skips; the unsupported-browser error path still runs. I/O or format
failures never cause a skip. Chromium and Firefox exercise persistence and
cross-tab ownership. WebKit persistence and real Safari/iOS qualification remain
pending; a green CI run does not imply that coverage.

## Current storage contract

`openArtifact({ name })` opens or creates `reddb-lite/<name>.rdb` inside the
origin's private filesystem. It exposes `append`, `readWal`, `checkpoint`,
`readSnapshot`, `generation`, `validateQuery` and `close`. These are artifact
primitives: WAL payloads are native binary records, and checkpoint images are
complete native store snapshots. Passing arbitrary SQL as a payload does not
execute it. The caller must incorporate committed WAL into a checkpoint image.

The browser and native file authority share the same format codecs and write
ordering. Acknowledgement follows both the WAL flush and root publication flush.
After a storage operation fails, that Rust handle rejects further I/O until it
is reopened and recovered. Existing corrupt files are preserved and reported;
they are never silently replaced. A second open returns `FILE_BUSY`; closing or
terminating the owning worker releases the lock. Invalid input and parse errors
do not invalidate the storage handle.

The host must provide exclusive ownership when using Rust's `EmbeddedRdbFile`
directly. The browser wrapper enforces this with Web Locks and OPFS's default
exclusive access handle. The prototype does not use relaxed/read-write-unsafe
access, emulate file locks, or require `SharedArrayBuffer` or isolation headers.

OPFS is subject to origin quota, storage eviction and explicit site-data clearing.
Applications can request persistent storage through `navigator.storage.persist()`;
these primitives do not promise that a browser will grant it or sync after closing.

## Remaining work toward the approved Lite design

1. Extract the storage-coupled executor/pager into a shared portable engine and
   preserve native query/transaction semantics for tables, documents, KV, graph
   and vectors. Port clocks, crypto randomness and task scheduling.
2. Build the application-facing SDK, local transaction/outbox atomicity, change
   observation, and one-owner request forwarding between tabs.
3. Implement versioned, policy-filtered snapshot and resumable change protocols
   in `reddb-wire` and the server. Revalidate normal authorization/RLS on upload;
   preserve transaction grouping, idempotency, stable graph references and deletes.
4. Add explicit conflict resolution, expired-cursor rebootstrap without losing
   pending intent, identity separation, offline assets and the storage-status UI.
5. Validate two-device sync and engine conformance, Safari/iOS on real devices,
   quota/failure cases, bundle size, startup, memory and query performance.
