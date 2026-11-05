# Dart and Flutter Web: local WASM database

Import `package:reddb/reddb_browser.dart` for browser use. The ordinary
`reddb.dart` entrypoint imports native TCP/process code and is not a web entrypoint.
This adapter is available in the repository source; it has not been released to pub.dev.
Pin the canonical release tag in your application's pubspec:

```yaml
dependencies:
  reddb:
    git:
      url: https://github.com/reddb-io/reddb.git
      ref: browser-v0.6.0
      path: drivers/dart
```

Install `@reddb-io/browser@0.6.0` in your web asset build and copy its
complete `src/` directory to your app's static assets, preserving the layout:

```sh
mkdir -p web/vendor/reddb-browser
cp -R node_modules/@reddb-io/browser/src/. web/vendor/reddb-browser/
```

Load the locally served module before starting your Dart/Flutter app, for example
in its HTML bootstrap (replace the final script with your app's bootstrap):

```html
<script type="module">
  import * as reddb from './vendor/reddb-browser/index.js'
  globalThis.reddbBrowserModule = reddb
  const script = document.createElement('script')
  script.src = './flutter_bootstrap.js'
  document.head.append(script)
</script>
```

```dart
import 'dart:js_interop';
import 'package:reddb/reddb_browser.dart';

@JS('reddbBrowserModule')
external JSObject get reddbModule;

Future<void> example() async {
  final session = await BrowserDatabaseSession.open(reddbModule);
  try {
    final db = await session.openDatabase(name: 'notes', create: true);
    try {
      await db.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)');
      await db.query('BEGIN');
      try {
        await db.query(r'INSERT INTO notes VALUES ($1, $2)', params: [1, 'Olá offline']);
        await db.query('COMMIT');
      } catch (_) {
        await db.query('ROLLBACK');
        rethrow;
      }
      print((await db.query('SELECT id, body FROM notes'))['rows']);
    } finally {
      await db.close();
    }
  } on BrowserRuntimeException catch (error) {
    print('${error.code}: ${error.message}');
  } finally {
    await session.close();
  }
}
```

The adapter uses `dart:js_interop`, with no FFI, native plugin or server process.
It delegates to the shared Rust/WASM Worker; it does not evaluate RQL in Dart.
`BrowserDatabaseSession` opens OPFS `.rdb` files and executes local table
SELECT/CREATE/INSERT/UPDATE/DELETE and BEGIN/COMMIT/ROLLBACK. `BrowserRuntime`
retains storage-free scalar execution. The same [browser support contract](../../docs/clients/browser.md)
applies: other collection models, AI and synchronization remain unavailable.
`validateQuery` checks syntax, not execution support.

Use HTTPS or localhost with OPFS and Web Locks. F5 and browser restarts retain
committed data within the same browser profile and origin. Private browsing,
site-data deletion and eviction can remove it. Request persistent storage and
export backups through the JS SDK where appropriate.

Close each database before reopening in the session. `db.close()` releases the
exclusive file lock and discards uncommitted changes; `session.close()` also
stops the Worker. Another owner fails with `FILE_BUSY`; old handles remain fenced.
Coordinate the entire open/read/change/COMMIT/close operation with an application
Web Lock across tabs. Serialize statements on each handle.

Creation defaults to false. `create: true` creates only missing files and never
replaces corrupt data. Parameters support null, booleans, safe JavaScript numbers
and strings. Bind values rather than interpolating SQL. Errors retain SDK codes,
including `STORAGE_UNSUPPORTED`, `FILE_BUSY`, `QUERY_UNSUPPORTED` and `CLOSED`.

Results are Dart maps/lists. Integers beyond JavaScript's safe range remain
canonical tagged maps, e.g. `{r'$int': '9007199254740993'}`; decimals also retain
the shared encoding. Do not cast those maps to `double` if exactness matters.
Runtime ABI/encoding mismatches fail with `VERSION_MISMATCH`; unsupported stored
queries fail with `QUERY_UNSUPPORTED`; operations after close fail with `CLOSED`.

Serve the JS, Worker and WASM locally. For offline reopening, include all of them
and the compiled Dart/Flutter application in your application's versioned cache.
The SDK does not install a service worker. There is no CDN fallback.
The Dart-to-JS adapter is tested in Chromium, Firefox and Playwright WebKit,
including stored SQL, rollback, lock contention, stale-handle fencing and offline reloads. Complete offline browser restarts are verified on
Chromium/Firefox; Playwright WPE persistent profiles lack working CacheStorage, so real
Safari/iOS qualification remains pending. Flutter widget integration and
Flutter's separate `--wasm` build mode are not qualified by that test.

Contributor verification from the repository root (Dart 3.13.7, .NET 8.0.426,
Node and the browser package build dependencies installed):

```sh
pnpm --filter @reddb-io/browser build
pnpm --filter @reddb-io/browser build:interop
pnpm --filter @reddb-io/browser test:interop
```
