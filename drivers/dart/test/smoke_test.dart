@Tags(['smoke'])
library;

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

/// End-to-end smoke. Skipped unless `RED_SMOKE=1` is set. Spawns the
/// project `red` binary directly via `Process.start` (no cargo build —
/// other agents may be holding the target dir).
void main() {
  final smoke = Platform.environment['RED_SMOKE'] == '1';
  if (!smoke) {
    test('smoke (skipped — set RED_SMOKE=1)', () {}, skip: 'RED_SMOKE not set');
    return;
  }

  late Process server;
  late int port;

  setUpAll(() async {
    final binary = Platform.environment['RED_BIN'] ?? 'red';
    final dataDir = await Directory.systemTemp.createTemp('reddb-dart-smoke-');
    port = await _freePort();
    server = await Process.start(binary, [
      'server',
      '--path',
      '${dataDir.path}/data.db',
      '--bind',
      '127.0.0.1:$port',
    ]);
    // Drain stderr so the process never blocks.
    server.stderr.listen((_) {});
    server.stdout.listen((_) {});
    final db = await _waitForConnect(port);
    await db.close();
  });

  tearDownAll(() async {
    server.kill(ProcessSignal.sigterm);
    await server.exitCode;
  });

  test('connect and query', () async {
    final db = await connect('red://127.0.0.1:$port');
    try {
      final raw = await db.query('SELECT 1');
      final decoded = jsonDecode(utf8.decode(raw));
      expect(decoded, isNotNull);
      await db.ping();
    } finally {
      await db.close();
    }
  }, timeout: const Timeout(Duration(seconds: 30)));

  test('parameterized query binds int text null and vector', () async {
    final db = await connect('red://127.0.0.1:$port');
    try {
      await db.query(
        'CREATE TABLE dart_params (id INT, name TEXT, nick TEXT, embedding VECTOR)',
      );
      await db.query(
        r'INSERT INTO dart_params '
        r'(id, name, nick, embedding) VALUES ($1, $2, $3, $4)',
        params: [1, 'alice', null, Float32List.fromList([0.1, 0.2, 0.3])],
      );
      final raw = await db.query(
        r'SELECT id, name, nick FROM dart_params WHERE id = $1 AND name = $2',
        params: [1, 'alice'],
      );
      final body = utf8.decode(raw);
      expect(body, contains('alice'));
    } finally {
      await db.close();
    }
  }, timeout: const Timeout(Duration(seconds: 30)));
}

Future<int> _freePort() async {
  final socket = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final port = socket.port;
  await socket.close();
  return port;
}

Future<Conn> _waitForConnect(int port) async {
  final deadline = DateTime.now().add(const Duration(seconds: 60));
  Object? last;
  while (DateTime.now().isBefore(deadline)) {
    try {
      final db = await connect('red://127.0.0.1:$port');
      try {
        await db.ping();
        return db;
      } catch (e) {
        await db.close();
        last = e;
      }
    } catch (e) {
      last = e;
    }
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
  throw StateError('server did not accept connections: $last');
}
