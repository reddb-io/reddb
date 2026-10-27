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
        'database': await databaseProbe(),
      }.jsify(),
    );
  } catch (error, stack) {
    failed('$error\n$stack'.toJS);
  } finally {
    await runtime?.close();
  }
}

Future<Object> databaseProbe() async {
  BrowserDatabaseSession? session;
  BrowserDatabaseSession? peer;
  try {
    session = await BrowserDatabaseSession.open(module);
    var db = await session.openDatabase(name: 'dart-interop', create: true);
    await db.query(
        'CREATE TABLE IF NOT EXISTS visits (id INTEGER PRIMARY KEY, body TEXT NOT NULL)');
    final before = (await db
        .query('SELECT body FROM visits WHERE id = 1'))['rows'] as List;
    final visits = before.isEmpty ? 0 : int.parse(before.single[0] as String);
    await db.query('BEGIN');
    await db.query('DELETE FROM visits');
    await db.query(r'INSERT INTO visits VALUES ($1, $2)',
        params: [1, '${visits + 1}']);
    await db.query(r'INSERT INTO visits VALUES ($1, $2)',
        params: [2, "offline ' português 🃏"]);
    await db.query('COMMIT');
    await db.query('BEGIN');
    await db.query("UPDATE visits SET body = 'rollback' WHERE id = 2");
    await db.query('ROLLBACK');
    final text = (await db
        .query('SELECT body FROM visits WHERE id = 2'))['rows'] as List;
    if (text.single[0] != "offline ' português 🃏") {
      throw StateError('Rollback changed committed data');
    }
    peer = await BrowserDatabaseSession.open(module);
    String? busy;
    try {
      await peer.openDatabase(name: 'dart-interop');
    } on BrowserRuntimeException catch (error) {
      busy = error.code;
    }
    if (busy != 'FILE_BUSY') {
      throw StateError('Lost exclusive file ownership: $busy');
    }
    await db.query('BEGIN');
    await db.query("UPDATE visits SET body = 'discard on close' WHERE id = 2");
    final old = db;
    await Future.wait([db.close(), db.close()]);
    db = await peer.openDatabase(name: 'dart-interop');
    final recovered = (await db
        .query('SELECT body FROM visits WHERE id = 2'))['rows'] as List;
    if (recovered.single[0] != "offline ' português 🃏") {
      throw StateError('Close published pending changes');
    }
    await db.close();
    db = await session.openDatabase(name: 'dart-interop');
    String? stale;
    try {
      await old.query('DELETE FROM visits');
    } on BrowserRuntimeException catch (error) {
      stale = error.code;
    }
    await old.close();
    if (stale != 'CLOSED') {
      throw StateError('Stale database handle remained usable');
    }
    final rows = (await db
        .query('SELECT body FROM visits WHERE id = 1'))['rows'] as List;
    if (rows.single[0] != '${visits + 1}') {
      throw StateError('Stale close released current file');
    }
    await session.close();
    await session.close();
    await db.close();
    String? closed;
    try {
      await db.query('SELECT 1');
    } on BrowserRuntimeException catch (error) {
      closed = error.code;
    }
    return {
      'visits': visits + 1,
      'busy': busy,
      'stale': stale,
      'closed': closed
    };
  } on BrowserRuntimeException catch (error) {
    if (error.code == 'STORAGE_UNSUPPORTED') return error.code;
    rethrow;
  } finally {
    await peer?.close();
    await session?.close();
  }
}
