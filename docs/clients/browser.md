# RedDB Browser for JavaScript and TypeScript

RedDB Browser is the local browser package, `@reddb-io/browser`. Your frontend
calls its JavaScript API; a dedicated Worker runs the Rust engine as WebAssembly
and owns an OPFS `.rdb` file. Local operations require no RedDB server, account,
CDN or network service. Rust is needed to develop the engine, not to use the package.

The canonical `0.1.0` distribution exposes artifact
persistence/recovery, RQL validation and storage-free scalar SELECT execution.
The source build additionally exposes `openDatabase({ name })` and
`database.query(sql)` for read-only SELECT over existing local table artifacts.
This addition is not yet in the published `0.1.0` package. SQL writes, application
transactions, other collection models, observation and optional synchronization
are still being integrated. `append` and `checkpoint`
accept native binary WAL/snapshot payloads; they do not execute SQL or provide a
save-game/CRUD API.

## Read a persisted local table (source build)

```ts
import { openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const database = await openDatabase({ name: 'my-app', worker: new RedDBWorker() })
try {
  console.log(await database.query(
    'SELECT rid, name, total FROM people WHERE total >= 10 ORDER BY total DESC LIMIT 20'
  ))
} finally { await database.close() }
```

The file must already exist in the application's OPFS namespace, with canonical
snapshot/WAL bytes and persisted table contracts. The application can import its
native `.rdb` under the same Web Lock before opening it. Opening and querying do
not mutate the artifact. Missing files return `DATABASE_NOT_FOUND`; a missing or
invalid catalog is an error, never inferred from rows. The handle owns its Worker
and excludes both database and raw-artifact owners of the same name.

This is a local file-owner read API. Server authorization and tenant/RLS filtering
must be applied before exporting an authorized subset for offline use. It does
not create a server identity or enforce server policies inside an untrusted origin.

The first read surface supports one named table, aliases, scalar projections,
JSON paths, WHERE, ORDER BY source fields/expressions and literal LIMIT/OFFSET.
It rejects writes, parameters, DISTINCT, aggregates/windows, joins/subqueries,
other models and TTL/retention. The [source API reference](https://github.com/reddb-io/reddb/tree/main/packages/browser#read-local-tables-from-jsts)
lists admitted functions, the materialization cap and exact-value/secret behavior.
Cache the application, SDK, Worker and WASM together to reopen and query after a
complete offline browser restart.

## Storage-independent runtime and other languages

For validation and scalar SELECTs without opening an OPFS file, use
`createRuntime({ worker? })` instead of `openArtifact`. Both expose `info()`,
`validateQuery(sql)`, `executeScalar(sql)` and `close()`. The runtime needs module
Workers and WebAssembly; OPFS and Web Locks are only required for artifacts.
`info()` reports ABI 1, `reddb-json-v1` value encoding and capabilities. Stored
queries report true in source builds with `storedQueryMode: 'read-only-tables'`;
transactions and sync remain false. The published `0.1.0` runtime reports stored
queries false. Capabilities describe the engine; storage-independent handles
still expose only validation and scalar execution.

The JSON-safe `createInteropRuntime` bridge is also used by source adapters for
[Dart/Flutter Web](https://github.com/reddb-io/reddb/blob/main/drivers/dart/BROWSER.md)
and [C#/Blazor](https://github.com/reddb-io/reddb/blob/main/drivers/dotnet/src/Reddb.Browser/README.md).
They run the same Rust/WASM scalar evaluator, preserving exact integer/decimal
tags. They are not published to pub.dev/NuGet and do not expose storage-backed
queries. See the [runtime contract](https://github.com/reddb-io/reddb/tree/main/packages/browser#shared-runtime-and-language-interop)
for the version check, structured error envelopes and offline asset requirements.

## Try the browser example

Open the [interactive playground](https://reddb-io.github.io/reddb/browser/).
It installs the published npm package and bundles JS, Worker and WASM with the page.
Run a scalar SELECT, append an opaque binary test marker, reopen the OPFS artifact,
then disable networking and reload once the offline cache is ready. The marker is
artifact I/O evidence, not a collection record or application transaction.

[Example source and local commands](https://github.com/reddb-io/reddb/tree/main/packages/browser-example).

## Install

```sh
npm add @reddb-io/browser
```

The package contains JS, TypeScript declarations, the Worker and `.wasm`; there
are no install scripts or native executables. Browser releases use independent
`browser-v…` tags and the `latest` npm channel.

## Vite, including React, Vue and Svelte applications

Create one local database handle in your application's persistence module. The
library owns the supplied Worker's lifetime; pass a fresh Worker per handle.

```ts
import { openArtifact } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const artifact = await openArtifact({
  name: 'my-app',
  worker: new RedDBWorker(),
})

try {
  const result = await artifact.executeScalar('SELECT 6 * 7 AS answer')
  console.log(result.rows) // [[42]]
  await artifact.validateQuery('SELECT * FROM saves') // parses only
} finally {
  await artifact.close()
}
```

A Vite TypeScript application should include its usual `vite/client` types
(e.g. `/// <reference types="vite/client" />` in `vite-env.d.ts`). Vite packages
the Worker and emits the WASM asset with the application build. Other bundlers
can supply a module Worker from their corresponding worker-loader mechanism.
The package's `./worker` export is the Worker entrypoint.

This module belongs behind your application data-access interface, not inside a
UI component's render cycle. React/Vue/Svelte can share the same service instance
and close it on application teardown. A second owner of the same named file
receives `FILE_BUSY`; cross-tab request forwarding is still pending.

## Plain JavaScript without a bundler

Copy `node_modules/@reddb-io/browser/src/` to your application's static assets,
keeping the directory layout intact, for example `public/vendor/reddb-browser/`.
Then import the local module:

```html
<script type="module">
  import { openArtifact } from '/vendor/reddb-browser/index.js'
  const artifact = await openArtifact({ name: 'my-app' })
  try {
    console.log(await artifact.executeScalar('SELECT 42 AS answer'))
  } finally {
    await artifact.close()
  }
</script>
```

The default entry resolves its sibling `worker.js`, which loads
`wasm/reddb_wasm.js` and `wasm/reddb_wasm_bg.wasm`. Serve JS as JavaScript and
WASM as `application/wasm`, over HTTPS or trusted localhost.

## Offline installation and browser storage

Bundle all assets with your application. For a PWA, include the application
shell, generated Worker JS and WASM in your own versioned offline cache. The SDK
does not register a service worker or control your application's update policy.
After those assets are cached, a fresh browser/Worker can reopen OPFS with the
network disabled. A packaged application can serve these assets locally.

The baseline requires module Workers, OPFS synchronous access and Web Locks;
SharedArrayBuffer and isolation headers are not required. Chromium/Firefox
persistence and offline restarts are tested. Safari/iOS persistence remains
unqualified. Unsupported hosts report `STORAGE_UNSUPPORTED`.

OPFS belongs to the application's origin. Clearing site data removes it; quota
and eviction policies apply. Applications can request persistent storage with
`navigator.storage.persist()` and inspect quota through `navigator.storage.estimate()`.

## Where it fits among the JS/TS packages

| Package | Execution and storage |
| --- | --- |
| `@reddb-io/browser` | Local Rust/WASM Worker + OPFS; source builds also expose read-only table SELECT |
| `@reddb-io/client` | Remote RedDB connection, including its browser transport |
| `@reddb-io/sdk` | Node.js/TS SDK with native embedded/runtime support |

In `labs-crimson-bytes`, the future RedDB adapter belongs behind `SaveBackend`.
In `labs-trickster`, it belongs behind `GameStorage` through Dart/JS interop.
Existing saves and IndexedDB implementations need migration and transactional
parity before replacement. This release does not migrate either game.
