import 'dart:js_interop';

import 'package:reddb/reddb_browser.dart';

@JS('reddbBrowserModule')
external JSObject get module;
@JS('probeComplete')
external void complete(JSAny? value);
@JS('probeFailed')
external void failed(JSString message);

Future<void> main() async {
  BrowserRuntime? runtime;
  try {
    runtime = await BrowserRuntime.open(module);
    final info = await runtime.info();
    await runtime.validateQuery('SELECT 1');
    final scalar = await runtime.executeScalar('SELECT 6 * 7 AS answer');
    final exact = await runtime.executeScalar(
      'SELECT 9007199254740993 AS exact',
    );
    String? unsupported;
    try {
      await runtime.executeScalar('SELECT * FROM people');
    } on BrowserRuntimeException catch (error) {
      unsupported = error.code;
    }
    await runtime.close();
    await runtime.close();
    String? closed;
    try {
      await runtime.info();
    } on BrowserRuntimeException catch (error) {
      closed = error.code;
    }
    complete(
      {
        'language': 'dart',
        'abi': info['abiVersion'],
        'storedQueries': (info['capabilities'] as Map)['storedQueries'],
        'answer': (scalar['rows'] as List)[0][0],
        'exact': (exact['rows'] as List)[0][0][r'$int'],
        'unsupported': unsupported,
        'closed': closed,
      }.jsify(),
    );
  } catch (error, stack) {
    failed('$error\n$stack'.toJS);
  } finally {
    await runtime?.close();
  }
}
