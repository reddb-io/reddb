# RedDB Browser — experimental persistence and query engine

This private package is the first building block of RedDB Browser. It runs the
canonical Rust `.rdb` artifact publication/recovery code, RQL parser and shared
scalar evaluator in a dedicated WebAssembly worker, persisting through a real
OPFS sync access handle.
It requires a secure context, module workers, OPFS and Web Locks.

The target is a fully offline local database with a JavaScript/TypeScript API.
The Rust/WASM engine ships with the package; local operation must not require a
RedDB server, account, CDN or external API. Applications bundle the JS, worker
and WASM assets and make them available locally or through their own offline
application cache. Sync is optional and must never gate local reads or writes.

**It is not a database SDK yet.** It executes storage-free scalar SELECTs;
queries over stored data, five-model local transactions, synchronization,
conflict resolution and forwarding between tabs remain pending. It must not be
published or substituted for `@reddb-io/client`.

## Build and verify

Install the repository-pinned Rust toolchain, its `wasm32-unknown-unknown` target,
and `wasm-bindgen-cli` **0.2.120**. Install workspace dependencies with pnpm.

```sh
pnpm --filter @reddb-io/browser build
pnpm --filter @reddb-io/browser exec playwright install --with-deps chromium firefox webkit
pnpm --filter @reddb-io/browser test
cargo test --locked -p reddb-io-file
cargo test --locked -p reddb-io-engine
```

The `WASM_BINDGEN` environment variable can point at the pinned CLI executable.
`REDDB_BROWSER_BROWSERS=chromium` restricts the local browser test matrix; CI runs all
three engines. Generated JS/WASM bindings are ignored and rebuilt from Rust.

The tests probe the browser's actual native APIs. If a port lacks synchronous
OPFS (currently Playwright's WPE WebKit runner), its persistence tests explicitly
report skips; the unsupported-browser error path still runs. I/O or format
failures never cause a skip. Chromium and Firefox exercise persistence and
cross-tab ownership. WebKit persistence and real Safari/iOS qualification remain
pending; a green CI run does not imply that coverage.

The conformance tests compile a separate test-only WASM artifact and execute the same
assertions as the native engine tests: scalar goldens/errors, compiled row
filtering/sorting/projection, joins, aggregation, windows, set operations and
cancellation. These do not require OPFS, so they run even on browser ports
without synchronous storage. The same worker also checks the shared 16 KiB page
layout, checksum corruption detection, freelist trunk recovery, pinned-page
eviction and dirty writeback. Native unit tests for these primitives run from
`reddb-engine`; native module paths retain the same types, with pager restart
coverage. Pager I/O, allocation fences, WAL/DWB ordering and MVCC are still native.
Native differential tests additionally compare
the scalar corpus to actual `RedDBRuntime` query results.

## Current query contract

```js
const artifact = await openArtifact({ name: 'example' })
await artifact.executeScalar('SELECT 2 + 3 * 4 AS answer')
// { columns: ['answer'], rows: [[14]] }
await artifact.close()
```

`executeScalar` uses the shared Rust evaluator, built-in type/operator/function
catalogs and output labels. It accepts storage-free scalar SELECT expressions
and literal LIMIT/OFFSET; unsupported clauses and stored-data queries fail
explicitly. It never reads or changes the artifact. Integer values outside JS's
safe range retain canonical `$int`/`$uint` tags instead of rounding. This small
entrypoint is an execution milestone, not the full RQL/storage runtime.

## Current storage contract

`openArtifact({ name })` opens or creates `reddb-browser/<name>.rdb` inside the
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

## Offline asset loading

The SDK does not register a service worker or control application updates. A
PWA should include `index.js`, `worker.js`, `wasm/reddb_wasm.js` and
`wasm/reddb_wasm_bg.wasm` from `src/` in its own versioned application cache,
along with its application shell. Those assets must remain available together
after installation. A locally packaged host can serve the same assets locally.
HTTPS or a trusted localhost context is required for OPFS and Web Locks.

The browser suite includes a test host's service worker and restarts the entire
browser with networking disabled. It reloads the JS/worker/WASM assets, recovers
OPFS data, writes again and executes a scalar query without reaching the HTTP
host. This verifies the current artifact/scalar surface offline; storage-backed
queries and application transactions still require the engine work below.

## Remaining work toward the approved Browser design

1. Extract the remaining storage-coupled executor/pager into the shared portable engine and
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
