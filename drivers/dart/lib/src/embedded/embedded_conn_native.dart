import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../conn.dart';
import '../errors.dart';
import '../url.dart';

const int _expectedAbiVersion = 1;

typedef _AbiVersionNative = Uint32 Function();
typedef _AbiVersion = int Function();
typedef _OpenNative = Int32 Function(
  Pointer<Utf8>,
  Pointer<Pointer<Void>>,
  Pointer<Pointer<Utf8>>,
);
typedef _Open = int Function(
  Pointer<Utf8>,
  Pointer<Pointer<Void>>,
  Pointer<Pointer<Utf8>>,
);
typedef _InvokeNative = Int32 Function(
  Pointer<Void>,
  Pointer<Utf8>,
  Pointer<Utf8>,
  Pointer<Pointer<Utf8>>,
  Pointer<Pointer<Utf8>>,
);
typedef _Invoke = int Function(
  Pointer<Void>,
  Pointer<Utf8>,
  Pointer<Utf8>,
  Pointer<Pointer<Utf8>>,
  Pointer<Pointer<Utf8>>,
);
typedef _CloseNative = Int32 Function(
  Pointer<Void>,
  Pointer<Pointer<Utf8>>,
);
typedef _Close = int Function(Pointer<Void>, Pointer<Pointer<Utf8>>);
typedef _StringFreeNative = Void Function(Pointer<Utf8>);
typedef _StringFree = void Function(Pointer<Utf8>);

Future<Conn> openEmbedded(ParsedUri uri) async {
  final abi = _NativeAbi.load();
  final path = uri.path;
  return _EmbeddedConn(abi, abi.open(path));
}

class _NativeAbi {
  _NativeAbi(this._library)
      : _open = _library.lookupFunction<_OpenNative, _Open>('reddb_open'),
        _invoke = _library.lookupFunction<_InvokeNative, _Invoke>(
          'reddb_invoke',
        ),
        _close = _library.lookupFunction<_CloseNative, _Close>('reddb_close'),
        _stringFree = _library.lookupFunction<_StringFreeNative, _StringFree>(
          'reddb_string_free',
        ) {
    final version = _library
        .lookupFunction<_AbiVersionNative, _AbiVersion>('reddb_abi_version')();
    if (version != _expectedAbiVersion) {
      throw EmbeddedUnsupported(
        'incompatible RedDB embedded ABI: expected '
        '$_expectedAbiVersion, found $version',
      );
    }
  }

  final DynamicLibrary _library;
  final _Open _open;
  final _Invoke _invoke;
  final _Close _close;
  final _StringFree _stringFree;

  static _NativeAbi load() {
    final configured = Platform.environment['REDDB_FFI_LIBRARY'];
    if (configured != null && configured.isNotEmpty) {
      return _NativeAbi(DynamicLibrary.open(configured));
    }
    final name = Platform.isWindows
        ? 'reddb_ffi.dll'
        : Platform.isMacOS
            ? 'libreddb_ffi.dylib'
            : 'libreddb_ffi.so';
    try {
      return _NativeAbi(DynamicLibrary.open(name));
    } on ArgumentError catch (error) {
      throw EmbeddedUnsupported(
        'RedDB embedded library was not found ($name): $error',
      );
    }
  }

  Pointer<Void> open(String? path) {
    final outHandle = calloc<Pointer<Void>>();
    final outError = calloc<Pointer<Utf8>>();
    final nativePath = path?.toNativeUtf8();
    try {
      final status = _open(
        nativePath ?? nullptr.cast<Utf8>(),
        outHandle,
        outError,
      );
      final message = _take(outError.value);
      if (status != 0) {
        throw EngineError(
          message.isEmpty ? 'reddb_open failed with status $status' : message,
        );
      }
      if (outHandle.value == nullptr) {
        throw ProtocolError('reddb_open returned a null handle');
      }
      return outHandle.value;
    } finally {
      if (nativePath != null) calloc.free(nativePath);
      calloc.free(outHandle);
      calloc.free(outError);
    }
  }

  Uint8List invoke(
    Pointer<Void> handle,
    String method,
    Map<String, Object?> parameters,
  ) {
    final nativeMethod = method.toNativeUtf8();
    final nativeParams = jsonEncode(parameters).toNativeUtf8();
    final outResult = calloc<Pointer<Utf8>>();
    final outError = calloc<Pointer<Utf8>>();
    try {
      final status = _invoke(
        handle,
        nativeMethod,
        nativeParams,
        outResult,
        outError,
      );
      final message = _take(outError.value);
      if (status != 0) {
        if (outResult.value != nullptr) _stringFree(outResult.value);
        throw EngineError(
          message.isEmpty ? 'reddb_invoke failed with status $status' : message,
        );
      }
      if (outResult.value == nullptr) {
        throw ProtocolError('reddb_invoke returned a null result');
      }
      return Uint8List.fromList(utf8.encode(_take(outResult.value)));
    } finally {
      calloc.free(nativeMethod);
      calloc.free(nativeParams);
      calloc.free(outResult);
      calloc.free(outError);
    }
  }

  void close(Pointer<Void> handle) {
    final outError = calloc<Pointer<Utf8>>();
    try {
      final status = _close(handle, outError);
      final message = _take(outError.value);
      if (status != 0) {
        throw EngineError(
          message.isEmpty ? 'reddb_close failed with status $status' : message,
        );
      }
    } finally {
      calloc.free(outError);
    }
  }

  String _take(Pointer<Utf8> value) {
    if (value == nullptr) return '';
    try {
      return value.toDartString();
    } finally {
      _stringFree(value);
    }
  }
}

class _EmbeddedConn implements Conn {
  _EmbeddedConn(this._abi, this._handle);

  final _NativeAbi _abi;
  Pointer<Void> _handle;

  @override
  Future<Uint8List> query(String sql, {List<Object?>? params}) async {
    final body = <String, Object?>{'sql': sql};
    if (params != null && params.isNotEmpty) body['params'] = params;
    return _invoke('query', body);
  }

  @override
  Future<Uint8List> insert(
    String collection,
    Map<String, dynamic> payload,
  ) async =>
      _invoke('insert', {'collection': collection, 'payload': payload});

  @override
  Future<Uint8List> bulkInsert(
    String collection,
    List<Map<String, dynamic>> rows,
  ) async =>
      _invoke('bulk_insert', {'collection': collection, 'payloads': rows});

  @override
  Future<Uint8List> get(String collection, String id) async =>
      _invoke('get', {'collection': collection, 'id': id});

  @override
  Future<Uint8List> delete(String collection, String id) async =>
      _invoke('delete', {'collection': collection, 'id': id});

  @override
  Future<void> ping() async {
    _invoke('health', const {});
  }

  @override
  Future<void> close() async {
    if (_handle == nullptr) return;
    final handle = _handle;
    _handle = nullptr;
    _abi.close(handle);
  }

  Uint8List _invoke(String method, Map<String, Object?> parameters) {
    if (_handle == nullptr) {
      throw StateError('embedded RedDB connection is closed');
    }
    return _abi.invoke(_handle, method, parameters);
  }
}
