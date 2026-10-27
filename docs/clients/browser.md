# RedDB Browser for JavaScript and TypeScript

RedDB Browser is the local browser package, `@reddb-io/browser`. Your frontend
calls its JavaScript API; a dedicated Worker runs the Rust engine as WebAssembly
and owns an OPFS `.rdb` file. Local operations require no RedDB server, account,
CDN or network service. Rust is needed to develop the engine, not to use the package.

Version `0.4.0` exposes `openDatabase`, local table CRUD and single-owner SQL
transactions, alongside artifact persistence/recovery, RQL validation and
storage-free scalar SELECT. Other collection models, observation and optional
synchronization remain pending. `append` and `checkpoint` retain their separate
low-level binary API; applications use `database.query(sql)` for SQL.

## Create and use a local database

```ts
import { openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const database = await openDatabase({
  name: 'my-app', create: true, worker: new RedDBWorker(),
})
try {
  await database.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
  await database.query('BEGIN')
  await database.query("INSERT INTO notes VALUES (1, 'Offline'), (2, 'Temporary')")
  await database.query("UPDATE notes SET body = 'Saved offline' WHERE id = 1")
  await database.query('DELETE FROM notes WHERE id = 2')
  await database.query('COMMIT')
  console.log(await database.query('SELECT id, body FROM notes ORDER BY id'))
} finally { await database.close() }
```

This example inserts fixed IDs; later runs should update the records or use new
IDs. `create: true` only initializes a missing file. Without it, a missing file
returns `DATABASE_NOT_FOUND`. Existing files use canonical snapshot/WAL bytes
and authoritative table contracts; corrupt files/catalogs are errors. SELECT
never mutates the artifact. Native `.rdb` files can also be imported under the
same Web Lock before opening. The handle owns its Worker and excludes other
SQL/raw-artifact owners of the same database.

Each mutation returns an `affectedRows` column. BEGIN stages changes privately,
including table creation; reads on that handle see its own writes. COMMIT makes
the batch durable, while ROLLBACK, close and worker termination discard staged
changes. A statement error aborts an explicit transaction until ROLLBACK. An
ambiguous I/O failure requires close/reopen to recover the committed state.
This is one exclusively owned session, without concurrent connections, savepoints
or explicit isolation options. BEGIN copies the committed resident image.

This is local file-owner access. Apply server authorization and tenant/RLS
filtering before exporting an authorized subset. The handle does not create a
server identity or enforce server policies inside an untrusted origin.

Table SELECT supports aliases, scalar projections, JSON paths, WHERE, source
ORDER BY and literal LIMIT/OFFSET. Writes share native schema, defaults,
generated columns, CHECK and uniqueness enforcement. Other collection models,
TTL/retention, AI/events, context-index maintenance, encrypted writes,
RETURNING/ON CONFLICT and advanced SELECT clauses remain unsupported. Each
source table and transaction action list is bounded at 100,000 rows/actions.
See the [source API reference](https://github.com/reddb-io/reddb/tree/main/packages/browser#local-table-sql-from-jsts)
for admitted functions and exact-value/secret behavior.

Cache the application, SDK, Worker and WASM together for offline startup.
Installed TS/Vite package tests create and mutate tables across two complete
offline browser restarts, with the HTTP listener stopped.

## Bind JavaScript/TypeScript values

Version 0.3.0 added positional parameters to the same local database handle:

```ts
const body = "O'Reilly's offline note"
await database.query('INSERT INTO notes (id, body) VALUES ($1, $2)', [1, body])
console.log(await database.query('SELECT id, body FROM notes WHERE id = $1', [1]))
```

The shared native binder substitutes typed AST literals, without interpolating
values into SQL text. `$1`/`?1` indexes start at 1 and must be contiguous; repeated
indexes reuse a value. Missing, extra or skipped positions raise `INVALID_PARAMS`
and abort an active SQL transaction until ROLLBACK. JavaScript argument validation
occurs before statement execution.

Pass `null`, boolean, string, finite fractional numbers, safe integer numbers or
`bigint` within i64/u64 bounds. Use `bigint` for integers outside JavaScript's safe
range; result values still use `reddb-json-v1` exact-number tags. Objects, arrays,
dates, binary values, named parameters and SQL identifiers are not admitted as
parameters. The count is capped at 65,536. LIMIT parameters must be positive and
OFFSET parameters nonnegative. See the [parameter contract](https://github.com/reddb-io/reddb/tree/main/packages/browser#positional-parameters).

## Export and restore a database

Version 0.4.0 provides complete native `.rdb` backups through the same JS/TS API:

```ts
import { restoreDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const backup = await database.exportBackup()
// Save backup outside this site's storage. For a selected File:
// const backup = new Uint8Array(await selectedFile.arrayBuffer())
const restored = await restoreDatabase({
  name: 'my-app-restored', backup, worker: new RedDBWorker(),
})
try { console.log(await restored.query('SELECT * FROM notes')) }
finally { await restored.close() }
```

Export reads the native file while its Worker retains exclusive ownership. It
includes committed catalog/data/WAL and excludes the active transaction's pending
changes, without committing or rolling back that transaction. Restore validates
through the native Rust engine and requires an unused name. Existing destinations
are preserved (`DATABASE_EXISTS`, or `FILE_BUSY` while open).

Both operations work offline and accept files up to 256 MiB. Invalid inputs are
rejected; ordinary failed restores clean up their new destination. Terminating
the Worker/browser during restore may leave an incomplete new file; retries do
not overwrite it. A physical backup can contain old/deleted bytes and provides
no extra encryption or filtering. Save it as sensitive data outside the origin;
storage persistence by itself is not a backup. File selection/download belongs
to the embedding application. Export/restore requires memory proportional to
the physical file, in addition to the engine's resident database.

## Storage-independent runtime and other languages

For validation and scalar SELECTs without opening an OPFS file, use
`createRuntime({ worker? })` instead of `openArtifact`. Both expose `info()`,
`validateQuery(sql)`, `executeScalar(sql)` and `close()`. The runtime needs module
Workers and WebAssembly; OPFS and Web Locks are only required for artifacts.
`info()` reports ABI 1, `reddb-json-v1` value encoding and capabilities. Stored
queries report true in version `0.4.0` with `storedQueryMode: 'read-write-tables'`,
`sqlWrites: true`, `transactions: true`, `queryParameters: true` and `databaseBackups: true`; sync remains false.
Capabilities describe the engine; storage-independent handles
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
The page creates an empty local `notes` table. Choose Inserir and execute the
statement, then use Ler, Alterar or Excluir. BEGIN/COMMIT/ROLLBACK controls operate
on the same real database. Disable networking after the offline cache is ready,
reload and query your committed notes again. Reopening or updating the page
discards changes without COMMIT. The older binary-marker artifact is preserved
under its separate filename.

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
| `@reddb-io/browser` | Local Rust/WASM Worker + OPFS, table CRUD and single-owner SQL transactions |
| `@reddb-io/client` | Remote RedDB connection, including its browser transport |
| `@reddb-io/sdk` | Node.js/TS SDK with native embedded/runtime support |

In `labs-crimson-bytes`, the future RedDB adapter belongs behind `SaveBackend`.
In `labs-trickster`, it belongs behind `GameStorage` through Dart/JS interop.
Existing saves and IndexedDB implementations need migration and transactional
parity before replacement. This release does not migrate either game.
