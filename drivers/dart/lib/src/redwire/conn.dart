import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import '../conn.dart';
import '../errors.dart';
import '../options.dart';
import 'codec.dart';
import 'frame.dart';

/// Subset of `dart:io` Socket APIs that both Socket and SecureSocket
/// implement. Lets us write a single client over either transport.
typedef _Stream = Socket;

/// RedWire client. One instance owns one TCP / TLS socket.
///
/// Operations are serialised internally — callers may invoke methods
/// concurrently but each request waits in line for the previous one
/// to finish receiving its response. Correlation ids are monotonic.
class RedwireConn implements Conn {
  RedwireConn._(this._socket, this._zstd) {
    _subscription = _socket.listen(
      _onData,
      onError: _onError,
      onDone: _onDone,
      cancelOnError: true,
    );
  }

  final _Stream _socket;
  final ZstdCodec? _zstd;
  late final StreamSubscription<Uint8List> _subscription;

  final BytesBuilder _buffer = BytesBuilder(copy: false);
  final List<Completer<Frame>> _waiters = <Completer<Frame>>[];
  final Lock _writeLock = Lock();

  Object? _error;
  bool _eof = false;
  bool _closed = false;
  int _nextCorr = 1;

  /// Open a RedWire connection: TCP (or TLS), magic preamble, Hello /
  /// HelloAck / AuthResponse / AuthOk.
  ///
  /// `host` and `port` are required. `tls` selects TLS vs plaintext.
  static Future<RedwireConn> connect({
    required String host,
    required int port,
    String? token,
    String clientName = 'reddb-dart/0.1.0',
    bool tls = false,
    TlsOptions? tlsOpts,
    Duration timeout = const Duration(seconds: 30),
    ZstdCodec? zstd,
  }) async {
    final socket = await _openSocket(
      host: host,
      port: port,
      tls: tls,
      tlsOpts: tlsOpts,
      timeout: timeout,
    );
    socket.setOption(SocketOption.tcpNoDelay, true);

    final conn = RedwireConn._(socket, zstd);
    try {
      await conn._handshake(
        token: token,
        clientName: clientName,
        timeout: timeout,
      );
      return conn;
    } catch (_) {
      await conn.close();
      rethrow;
    }
  }

  static Future<_Stream> _openSocket({
    required String host,
    required int port,
    required bool tls,
    TlsOptions? tlsOpts,
    required Duration timeout,
  }) async {
    if (!tls) {
      return Socket.connect(host, port, timeout: timeout);
    }
    final ctx = tlsOpts?.context ?? SecurityContext.defaultContext;
    return SecureSocket.connect(
      host,
      port,
      context: ctx,
      onBadCertificate: tlsOpts?.allowInsecure == true ? (_) => true : null,
      supportedProtocols: tlsOpts?.alpnProtocols ?? const ['redwire/1'],
      timeout: timeout,
    );
  }

  Future<void> _handshake({
    required String? token,
    required String clientName,
    required Duration timeout,
  }) async {
    // Magic preamble.
    _socket.add([MAGIC, SUPPORTED_VERSION]);
    await _socket.flush();

    // Hello.
    final methods = token != null ? ['bearer'] : ['anonymous', 'bearer'];
    final hello = utf8.encode(jsonEncode({
      'versions': [SUPPORTED_VERSION],
      'auth_methods': methods,
      'features': 0,
      'client_name': clientName,
    }));
    await _writeFrame(MessageKind.hello, _next(), Uint8List.fromList(hello));

    final ack = await _readFrame(timeout);
    if (ack.kind == MessageKind.authFail) {
      throw AuthRefused(_jsonReason(ack.payload) ?? 'AuthFail at HelloAck');
    }
    if (ack.kind != MessageKind.helloAck) {
      throw ProtocolError(
        'expected HelloAck, got ${MessageKind.name(ack.kind)}',
      );
    }
    final ackParsed = _jsonOf(ack.payload);
    final chosen = ackParsed is Map ? ackParsed['auth'] : null;
    if (chosen is! String) {
      throw ProtocolError('HelloAck missing `auth` field');
    }

    Uint8List respPayload;
    if (chosen == 'anonymous') {
      respPayload = Uint8List(0);
    } else if (chosen == 'bearer') {
      if (token == null) {
        throw AuthRefused(
          'server demanded bearer but no token was supplied',
        );
      }
      respPayload = Uint8List.fromList(
        utf8.encode(jsonEncode({'token': token})),
      );
    } else {
      throw ProtocolError(
        "server picked unsupported auth method: '$chosen'",
      );
    }
    await _writeFrame(MessageKind.authResponse, _next(), respPayload);

    final fin = await _readFrame(timeout);
    if (fin.kind == MessageKind.authFail) {
      throw AuthRefused(_jsonReason(fin.payload) ?? 'auth refused');
    }
    if (fin.kind != MessageKind.authOk) {
      throw ProtocolError(
        'expected AuthOk, got ${MessageKind.name(fin.kind)}',
      );
    }
  }

  // ---------------------------------------------------------------------
  // Public Conn surface
  // ---------------------------------------------------------------------

  @override
  Future<Uint8List> query(String sql) async {
    final resp = await _request(
      MessageKind.query,
      Uint8List.fromList(utf8.encode(sql)),
    );
    return _expectResultOrError(resp);
  }

  @override
  Future<Uint8List> insert(
    String collection,
    Map<String, dynamic> payload,
  ) async {
    final body = utf8.encode(jsonEncode({
      'collection': collection,
      'payload': payload,
    }));
    final resp = await _request(MessageKind.bulkInsert, Uint8List.fromList(body));
    return _expectBulkOrError(resp);
  }

  @override
  Future<Uint8List> bulkInsert(
    String collection,
    List<Map<String, dynamic>> rows,
  ) async {
    final body = utf8.encode(jsonEncode({
      'collection': collection,
      'payloads': rows,
    }));
    final resp = await _request(MessageKind.bulkInsert, Uint8List.fromList(body));
    return _expectBulkOrError(resp);
  }

  @override
  Future<Uint8List> get(String collection, String id) async {
    final body = utf8.encode(jsonEncode({'collection': collection, 'id': id}));
    final resp = await _request(MessageKind.get, Uint8List.fromList(body));
    return _expectResultOrError(resp);
  }

  @override
  Future<Uint8List> delete(String collection, String id) async {
    final body = utf8.encode(jsonEncode({'collection': collection, 'id': id}));
    final resp = await _request(MessageKind.delete, Uint8List.fromList(body));
    if (resp.kind == MessageKind.deleteOk) return resp.payload;
    if (resp.kind == MessageKind.error) {
      throw EngineError(utf8.decode(resp.payload, allowMalformed: true));
    }
    throw ProtocolError(
      'expected DeleteOk/Error, got ${MessageKind.name(resp.kind)}',
    );
  }

  @override
  Future<void> ping() async {
    final resp = await _request(MessageKind.ping, Uint8List(0));
    if (resp.kind != MessageKind.pong) {
      throw ProtocolError(
        'expected Pong, got ${MessageKind.name(resp.kind)}',
      );
    }
  }

  @override
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    try {
      await _writeFrame(MessageKind.bye, _next(), Uint8List(0));
    } catch (_) {
      // best-effort
    }
    try {
      await _socket.close();
    } catch (_) {
      // ignore
    }
    await _subscription.cancel();
    final err = _error ?? RedDBError('CONNECTION_CLOSED', 'connection closed');
    while (_waiters.isNotEmpty) {
      _waiters.removeAt(0).completeError(err);
    }
  }

  // ---------------------------------------------------------------------
  // Internal helpers
  // ---------------------------------------------------------------------

  int _next() {
    final c = _nextCorr;
    _nextCorr = (_nextCorr + 1) & 0x7FFFFFFFFFFFFFFF;
    return c;
  }

  Future<Frame> _request(int kind, Uint8List payload) async {
    if (_closed) {
      throw RedDBError('CONNECTION_CLOSED', 'connection already closed');
    }
    return _writeLock.synchronized(() async {
      final corr = _next();
      await _writeFrame(kind, corr, payload);
      return _readFrame(const Duration(seconds: 30));
    });
  }

  Future<void> _writeFrame(int kind, int corr, Uint8List payload) async {
    final frame = Frame(kind: kind, correlationId: corr, payload: payload);
    final bytes = encodeFrame(frame, zstd: _zstd);
    _socket.add(bytes);
    await _socket.flush();
  }

  Future<Frame> _readFrame(Duration timeout) {
    if (_error != null) return Future.error(_error!);
    final c = Completer<Frame>();
    _waiters.add(c);
    _tryDeliver();
    if (_eof && !c.isCompleted) {
      _flushWaitersWithEof();
    }
    return c.future.timeout(timeout, onTimeout: () {
      _waiters.remove(c);
      throw RedDBError('TIMEOUT', 'redwire read timed out after $timeout');
    });
  }

  void _onData(Uint8List chunk) {
    _buffer.add(chunk);
    _tryDeliver();
  }

  void _onError(Object err, [StackTrace? st]) {
    _error = err;
    _flushWaitersWithError(err);
  }

  void _onDone() {
    _eof = true;
    _flushWaitersWithEof();
  }

  void _tryDeliver() {
    while (_waiters.isNotEmpty && _buffer.length >= FRAME_HEADER_SIZE) {
      final all = _buffer.toBytes();
      ({Frame frame, int consumed})? r;
      try {
        r = decodeFrame(all, zstd: _zstd);
      } catch (e) {
        _waiters.removeAt(0).completeError(e);
        // Keep remaining bytes — bad frame is unrecoverable but
        // return the rest to the caller's later reads.
        _buffer.clear();
        _buffer.add(all);
        return;
      }
      if (r == null) {
        // Need more bytes — re-buffer the flattened buf so we don't
        // keep flattening repeatedly.
        _buffer.clear();
        _buffer.add(all);
        return;
      }
      final remaining = Uint8List.sublistView(all, r.consumed);
      _buffer.clear();
      if (remaining.isNotEmpty) {
        _buffer.add(remaining);
      }
      _waiters.removeAt(0).complete(r.frame);
    }
  }

  void _flushWaitersWithEof() {
    if (_waiters.isEmpty) return;
    if (_buffer.length > 0) {
      _tryDeliver();
      return;
    }
    final err = _error ?? RedDBError('CONNECTION_CLOSED', 'connection closed');
    _flushWaitersWithError(err);
  }

  void _flushWaitersWithError(Object err) {
    while (_waiters.isNotEmpty) {
      _waiters.removeAt(0).completeError(err);
    }
  }

  Uint8List _expectResultOrError(Frame resp) {
    if (resp.kind == MessageKind.result) return resp.payload;
    if (resp.kind == MessageKind.error) {
      throw EngineError(utf8.decode(resp.payload, allowMalformed: true));
    }
    throw ProtocolError(
      'expected Result/Error, got ${MessageKind.name(resp.kind)}',
    );
  }

  Uint8List _expectBulkOrError(Frame resp) {
    if (resp.kind == MessageKind.bulkOk) return resp.payload;
    if (resp.kind == MessageKind.error) {
      throw EngineError(utf8.decode(resp.payload, allowMalformed: true));
    }
    throw ProtocolError(
      'expected BulkOk/Error, got ${MessageKind.name(resp.kind)}',
    );
  }

  static Object? _jsonOf(Uint8List bytes) {
    if (bytes.isEmpty) return null;
    try {
      return jsonDecode(utf8.decode(bytes));
    } catch (_) {
      return null;
    }
  }

  static String? _jsonReason(Uint8List bytes) {
    final v = _jsonOf(bytes);
    if (v is Map && v['reason'] is String) return v['reason'] as String;
    return null;
  }
}

/// Tiny mutex — Dart doesn't ship one in the SDK and we only need
/// FIFO serialised access for the single socket.
class Lock {
  Future<void> _last = Future.value();

  Future<T> synchronized<T>(Future<T> Function() body) {
    final completer = Completer<T>();
    final prev = _last;
    _last = completer.future
        .then<void>((_) {})
        .catchError((Object _, StackTrace __) {});
    prev.whenComplete(() async {
      try {
        completer.complete(await body());
      } catch (e, st) {
        completer.completeError(e, st);
      }
    });
    return completer.future;
  }
}
