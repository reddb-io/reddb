import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:reddb/reddb.dart';
import 'package:reddb/src/redwire/conn.dart';
import 'package:reddb/src/redwire/frame.dart' as f;
import 'package:test/test.dart';

/// Minimal fake server. Spawns a `ServerSocket` on `127.0.0.1:0` and
/// runs a hand-written state machine that mimics the engine's
/// handshake — enough to exercise the client end-to-end.
class _FakeServer {
  _FakeServer._(this._sock);

  final ServerSocket _sock;

  String get host => _sock.address.address;
  int get port => _sock.port;

  static Future<_FakeServer> start(
    Future<void> Function(Socket s, _FrameReader r) handler,
  ) async {
    final s = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final fake = _FakeServer._(s);
    s.listen((sock) {
      final r = _FrameReader(sock);
      handler(sock, r).whenComplete(() async {
        try {
          await sock.close();
        } catch (_) {}
        await r.close();
      });
    });
    return fake;
  }

  Future<void> close() => _sock.close();
}

/// Server-side frame reader. Buffers raw bytes and exposes two await
/// points: the 2-byte preamble, then frames one at a time.
class _FrameReader {
  _FrameReader(Socket sock) {
    _sub = sock.listen((data) {
      _buf.add(data);
      _drain();
    }, onError: (Object e, StackTrace st) {
      _err = e;
      _drainErrors(e);
    }, onDone: () {
      _eof = true;
      _drainErrors(StateError('eof'));
    });
  }

  late final StreamSubscription<Uint8List> _sub;
  final BytesBuilder _buf = BytesBuilder(copy: false);
  final List<_Pending> _waiters = [];
  bool _eof = false;
  Object? _err;

  Future<Uint8List> readPreamble() {
    final c = Completer<Uint8List>();
    _waiters.add(_Pending.preamble(c));
    _drain();
    return c.future;
  }

  Future<f.Frame> readFrame() {
    final c = Completer<f.Frame>();
    _waiters.add(_Pending.frame(c));
    _drain();
    return c.future;
  }

  void _drain() {
    while (_waiters.isNotEmpty && _buf.length > 0) {
      final w = _waiters.first;
      if (w.kind == _PendingKind.preamble) {
        if (_buf.length < 2) return;
        _waiters.removeAt(0);
        final bytes = _buf.toBytes();
        _buf.clear();
        _buf.add(Uint8List.sublistView(bytes, 2));
        w.preambleCompleter!.complete(Uint8List.sublistView(bytes, 0, 2));
        continue;
      }
      // frame
      if (_buf.length < f.FRAME_HEADER_SIZE) return;
      final bytes = _buf.toBytes();
      ({f.Frame frame, int consumed})? r;
      try {
        r = f.decodeFrame(bytes);
      } catch (e) {
        _waiters.removeAt(0);
        _buf.clear();
        w.frameCompleter!.completeError(e);
        continue;
      }
      if (r == null) {
        _buf.clear();
        _buf.add(bytes);
        return;
      }
      _waiters.removeAt(0);
      final remaining = Uint8List.sublistView(bytes, r.consumed);
      _buf.clear();
      if (remaining.isNotEmpty) _buf.add(remaining);
      w.frameCompleter!.complete(r.frame);
    }
    if (_eof || _err != null) {
      _drainErrors(_err ?? StateError('eof'));
    }
  }

  void _drainErrors(Object err) {
    while (_waiters.isNotEmpty) {
      final w = _waiters.removeAt(0);
      if (w.kind == _PendingKind.preamble) {
        if (!w.preambleCompleter!.isCompleted) {
          w.preambleCompleter!.completeError(err);
        }
      } else {
        if (!w.frameCompleter!.isCompleted) {
          w.frameCompleter!.completeError(err);
        }
      }
    }
  }

  Future<void> close() async {
    await _sub.cancel();
  }
}

enum _PendingKind { preamble, frame }

class _Pending {
  _Pending.preamble(Completer<Uint8List> c)
      : kind = _PendingKind.preamble,
        preambleCompleter = c,
        frameCompleter = null;
  _Pending.frame(Completer<f.Frame> c)
      : kind = _PendingKind.frame,
        preambleCompleter = null,
        frameCompleter = c;

  final _PendingKind kind;
  final Completer<Uint8List>? preambleCompleter;
  final Completer<f.Frame>? frameCompleter;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

Future<void> _writeFrameOnSocket(
  Socket s,
  int kind,
  int corr,
  Uint8List payload,
) async {
  final frame = f.Frame(kind: kind, correlationId: corr, payload: payload);
  s.add(f.encodeFrame(frame));
  await s.flush();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

void main() {
  group('RedwireConn handshake', () {
    test('anonymous handshake → query → ping → close', () async {
      final server = await _FakeServer.start((sock, reader) async {
        // Magic preamble.
        final preamble = await reader.readPreamble();
        expect(preamble[0], MAGIC);
        expect(preamble[1], SUPPORTED_VERSION);

        // Hello.
        final hello = await reader.readFrame();
        expect(hello.kind, MessageKind.hello);

        // HelloAck → anonymous.
        await _writeFrameOnSocket(
          sock,
          MessageKind.helloAck,
          hello.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'auth': 'anonymous'}))),
        );

        // AuthResponse (empty body for anonymous).
        final authResp = await reader.readFrame();
        expect(authResp.kind, MessageKind.authResponse);

        // AuthOk.
        await _writeFrameOnSocket(
          sock,
          MessageKind.authOk,
          authResp.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'session_id': 'sess-1'}))),
        );

        // Loop on requests.
        try {
          while (true) {
            final req = await reader.readFrame();
            if (req.kind == MessageKind.bye) break;
            if (req.kind == MessageKind.ping) {
              await _writeFrameOnSocket(
                sock,
                MessageKind.pong,
                req.correlationId,
                Uint8List(0),
              );
            } else if (req.kind == MessageKind.query) {
              await _writeFrameOnSocket(
                sock,
                MessageKind.result,
                req.correlationId,
                Uint8List.fromList(
                  utf8.encode(jsonEncode(<String, Object?>{
                    'records': <Object?>[],
                    'echo': utf8.decode(req.payload),
                  })),
                ),
              );
            }
          }
        } catch (_) {
          // disconnected
        }
      });

      try {
        final conn = await RedwireConn.connect(host: server.host, port: server.port);
        final raw = await conn.query('SELECT 1');
        final decoded = jsonDecode(utf8.decode(raw)) as Map<String, dynamic>;
        expect(decoded['echo'], 'SELECT 1');
        await conn.ping();
        await conn.close();
      } finally {
        await server.close();
      }
    });

    test('bearer handshake', () async {
      const expectedToken = 'sk-test-token';
      final server = await _FakeServer.start((sock, reader) async {
        await reader.readPreamble();
        final hello = await reader.readFrame();
        expect(hello.kind, MessageKind.hello);
        await _writeFrameOnSocket(
          sock,
          MessageKind.helloAck,
          hello.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'auth': 'bearer'}))),
        );
        final authResp = await reader.readFrame();
        final body = jsonDecode(utf8.decode(authResp.payload)) as Map<String, dynamic>;
        expect(body['token'], expectedToken);
        await _writeFrameOnSocket(
          sock,
          MessageKind.authOk,
          authResp.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'session_id': 's2'}))),
        );
        try {
          final bye = await reader.readFrame();
          expect(bye.kind, MessageKind.bye);
        } catch (_) {
          // socket closed before bye landed — fine.
        }
      });
      try {
        final conn = await RedwireConn.connect(
          host: server.host,
          port: server.port,
          token: expectedToken,
        );
        await conn.close();
      } finally {
        await server.close();
      }
    });

    test('AuthFail at HelloAck surfaces AuthRefused', () async {
      final server = await _FakeServer.start((sock, reader) async {
        await reader.readPreamble();
        final hello = await reader.readFrame();
        await _writeFrameOnSocket(
          sock,
          MessageKind.authFail,
          hello.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'reason': 'no anon allowed'}))),
        );
      });
      try {
        await expectLater(
          RedwireConn.connect(host: server.host, port: server.port),
          throwsA(isA<AuthRefused>()),
        );
      } finally {
        await server.close();
      }
    });

    test('AuthFail at AuthOk surfaces AuthRefused', () async {
      final server = await _FakeServer.start((sock, reader) async {
        await reader.readPreamble();
        final hello = await reader.readFrame();
        await _writeFrameOnSocket(
          sock,
          MessageKind.helloAck,
          hello.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'auth': 'anonymous'}))),
        );
        final authResp = await reader.readFrame();
        await _writeFrameOnSocket(
          sock,
          MessageKind.authFail,
          authResp.correlationId,
          Uint8List.fromList(utf8.encode(jsonEncode({'reason': 'rejected'}))),
        );
      });
      try {
        await expectLater(
          RedwireConn.connect(host: server.host, port: server.port),
          throwsA(isA<AuthRefused>()),
        );
      } finally {
        await server.close();
      }
    });
  });
}
