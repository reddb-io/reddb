import 'dart:js_interop';

extension type _Module(JSObject _) implements JSObject {
  external _Bridge createInteropRuntime();
}

extension type _Bridge(JSObject _) implements JSObject {
  external JSPromise<JSObject> invoke(JSString method, JSObject params);
  external void close();
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

class BrowserRuntimeException implements Exception {
  final String code;
  final String message;
  BrowserRuntimeException(this.code, this.message);

  @override
  String toString() => 'BrowserRuntimeException($code): $message';
}

/// A storage-independent worker using the application's locally bundled JS module.
/// Exact integers/decimals retain RedDB's tagged JSON representation.
class BrowserRuntime {
  final _Bridge _bridge;
  BrowserRuntime._(this._bridge);

  static Future<BrowserRuntime> open(JSObject module) async {
    final runtime = BrowserRuntime._(_Module(module).createInteropRuntime());
    try {
      final contract = await runtime.info();
      if (contract['abiVersion'] != 1 ||
          contract['valueEncoding'] != 'reddb-json-v1') {
        throw BrowserRuntimeException(
          'VERSION_MISMATCH',
          'Unsupported runtime ABI',
        );
      }
      return runtime;
    } catch (_) {
      await runtime.close();
      rethrow;
    }
  }

  Future<Object?> _invoke(
    String method, [
    Map<String, Object?> params = const {},
  ]) async {
    final response = _Envelope(
      await _bridge.invoke(method.toJS, params.jsify() as JSObject).toDart,
    );
    if (!response.ok.toDart) {
      throw BrowserRuntimeException(
        response.error.code.toDart,
        response.error.message.toDart,
      );
    }
    return response.result?.dartify();
  }

  Future<Map<String, Object?>> info() async =>
      Map<String, Object?>.from(await _invoke('info') as Map);

  Future<void> validateQuery(String sql) async {
    await _invoke('validateQuery', {'sql': sql});
  }

  Future<Map<String, Object?>> executeScalar(String sql) async =>
      Map<String, Object?>.from(
        await _invoke('executeScalar', {'sql': sql}) as Map,
      );

  Future<void> close() async => _bridge.close();
}
