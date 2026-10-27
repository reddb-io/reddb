import 'dart:js_interop';

import 'runtime.dart';

extension type _Module(JSObject _) implements JSObject {
  external _Bridge createInteropDatabaseSession();
}

extension type _Bridge(JSObject _) implements JSObject {
  external JSPromise<_Envelope> invoke(JSString method, JSObject params);
}

extension type _Envelope(JSObject _) implements JSObject {
  external JSBoolean get ok;
  external JSAny? get result;
  external _Failure get error;
}

extension type _Failure(JSObject _) implements JSObject {
  external JSString get code;
  external JSString get message;
}

/// Retains one locally bundled Rust/WASM Worker between database operations.
/// Close each database before opening another; close the session when finished.
class BrowserDatabaseSession {
  final _Bridge _bridge;
  BrowserDatabaseSession._(this._bridge);

  static Future<BrowserDatabaseSession> open(JSObject module) async {
    final session = BrowserDatabaseSession._(
      _Module(module).createInteropDatabaseSession(),
    );
    await session._invoke('ready');
    return session;
  }

  Future<Object?> _invoke(
    String method, [
    Map<String, Object?> params = const {},
  ]) async {
    final response =
        await _bridge.invoke(method.toJS, params.jsify() as JSObject).toDart;
    if (!response.ok.toDart) {
      throw BrowserRuntimeException(
        response.error.code.toDart,
        response.error.message.toDart,
      );
    }
    return response.result?.dartify();
  }

  /// Open an OPFS database. Creation is explicit and never replaces corruption.
  Future<BrowserDatabase> openDatabase({
    required String name,
    bool create = false,
  }) async {
    final handle = await _invoke('openDatabase', {
      'name': name,
      'create': create,
    });
    return BrowserDatabase._(this, (handle as num).toInt());
  }

  /// Releases the current file, discards uncommitted changes and stops the Worker.
  Future<void> close() async => _invoke('close');
}

/// Local table SQL, positional parameters and BEGIN/COMMIT/ROLLBACK.
/// Exact query results keep RedDB's tagged JSON values, as in BrowserRuntime.
class BrowserDatabase {
  final BrowserDatabaseSession _session;
  final int _handle;
  BrowserDatabase._(this._session, this._handle);

  Future<Map<String, Object?>> info() async => Map<String, Object?>.from(
        await _session._invoke('info', {'handle': _handle}) as Map,
      );

  /// Parameters support null, bool, safe JS numbers and String values.
  /// Serialize transactions on this handle; use an application Web Lock to
  /// coordinate the complete open/read/change/commit/close cycle across tabs.
  Future<Map<String, Object?>> query(
    String sql, {
    List<Object?> params = const [],
  }) async =>
      Map<String, Object?>.from(
        await _session._invoke('query', {
          'handle': _handle,
          'sql': sql,
          'params': params,
        }) as Map,
      );

  /// Releases file ownership, retaining the session's initialized Worker.
  Future<void> close() async => _session._invoke('closeDatabase', {
        'handle': _handle,
      });
}
