# Dart and Flutter Web: experimental local WASM runtime

Import `package:reddb/reddb_browser.dart` for browser use. The ordinary
`reddb.dart` entrypoint imports native TCP/process code and is not a web entrypoint.
This adapter is available in the repository source; it has not been released to pub.dev.

Install `@reddb-io/browser@experimental` in your web asset build and copy its
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
  final runtime = await BrowserRuntime.open(reddbModule);
  try {
    final info = await runtime.info();
    final result = await runtime.executeScalar('SELECT 6 * 7 AS answer');
    print(info['capabilities']);
    print(result['rows']); // [[42]]
  } on BrowserRuntimeException catch (error) {
    print('${error.code}: ${error.message}');
  } finally {
    await runtime.close();
  }
}
```

The adapter uses `dart:js_interop`, with no FFI, native plugin or server process.
It delegates to the shared Rust/WASM Worker; it does not evaluate RQL in Dart.
The current API validates RQL and executes storage-free scalar SELECTs.
Queries over persisted collections, application transactions and sync are not
available. `validateQuery` only checks syntax, not execution support.

Results are Dart maps/lists. Integers beyond JavaScript's safe range remain
canonical tagged maps, e.g. `{r'$int': '9007199254740993'}`; decimals also retain
the shared encoding. Do not cast those maps to `double` if exactness matters.
Runtime ABI/encoding mismatches fail with `VERSION_MISMATCH`; unsupported stored
queries fail with `QUERY_UNSUPPORTED`; operations after close fail with `CLOSED`.

Serve the JS, Worker and WASM locally. For offline reopening, include all of them
and the compiled Dart/Flutter application in your application's versioned cache.
The SDK does not install a service worker. There is no CDN fallback.
The Dart-to-JS adapter is tested in Chromium, Firefox and Playwright WebKit,
including offline reloads. Complete offline browser restarts are verified on
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
