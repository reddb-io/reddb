@Tags(['smoke'])
library;

import 'dart:convert';
import 'dart:io';

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
    port = 5050 + (DateTime.now().millisecondsSinceEpoch % 1000);
    server = await Process.start(binary, ['server', '--port', port.toString()]);
    // Give it a moment to bind.
    await Future<void>.delayed(const Duration(milliseconds: 500));
    // Drain stderr so the process never blocks.
    server.stderr.listen((_) {});
    server.stdout.listen((_) {});
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
}
