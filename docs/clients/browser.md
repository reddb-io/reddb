# RedDB Browser for JavaScript and TypeScript

RedDB Browser is the local browser package, `@reddb-io/browser`. Your frontend
calls its JavaScript API; a dedicated Worker runs the Rust engine as WebAssembly
and owns an OPFS `.rdb` file. Local operations require no RedDB server, account,
CDN or network service. Rust is needed to develop the engine, not to use the package.

The first distribution, `0.1.0-alpha.0`, is **experimental**. It exposes artifact
persistence/recovery, RQL validation and storage-free scalar SELECT execution.
Queries over stored collections, application transactions, observation and
optional synchronization are still being integrated. `append` and `checkpoint`
accept native binary WAL/snapshot payloads; they do not execute SQL or provide a
save-game/CRUD API.

## Try the browser example

Open the [interactive playground](https://reddb-io.github.io/reddb/browser/).
It installs the published npm package and bundles JS, Worker and WASM with the page.
Run a scalar SELECT, append an opaque binary test marker, reopen the OPFS artifact,
then disable networking and reload once the offline cache is ready. The marker is
artifact I/O evidence, not a collection record or application transaction.

[Example source and local commands](https://github.com/reddb-io/reddb/tree/main/packages/browser-example).

## Install

```sh
npm add @reddb-io/browser@experimental
```

The package contains JS, TypeScript declarations, the Worker and `.wasm`; there
are no install scripts or native executables. Browser releases use independent
`browser-v…` tags and the `experimental` npm channel.

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
| `@reddb-io/browser` | Local Rust/WASM Worker + OPFS; current experimental artifact/scalar surface |
| `@reddb-io/client` | Remote RedDB connection, including its browser transport |
| `@reddb-io/sdk` | Node.js/TS SDK with native embedded/runtime support |

In `labs-crimson-bytes`, the future RedDB adapter belongs behind `SaveBackend`.
In `labs-trickster`, it belongs behind `GameStorage` through Dart/JS interop.
Existing saves and IndexedDB implementations need migration and transactional
parity before replacement. This experimental release does not migrate either game.
