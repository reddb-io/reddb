@Tags(['embedded'])
library;

import 'dart:convert';
import 'dart:io';

import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

void main() {
  final enabled = Platform.environment['REDDB_EMBEDDED_SMOKE'] == '1';
  if (!enabled) {
    test('embedded (skipped — set REDDB_EMBEDDED_SMOKE=1)', () {},
        skip: 'native ABI not requested');
    return;
  }

  test('memory parameters and single-file reopen use the native ABI', () async {
    final memory = await connect('memory://');
    try {
      await memory.query('CREATE TABLE dart_embedded (id INTEGER, name TEXT)');
      await memory.query(
        r'INSERT INTO dart_embedded (id, name) VALUES ($1, $2)',
        params: [7, 'embedded'],
      );
      final rows = await memory.query(
        r'SELECT name FROM dart_embedded WHERE id = $1',
        params: [7],
      );
      expect(utf8.decode(rows), contains('embedded'));
    } finally {
      await memory.close();
    }

    final directory =
        await Directory.systemTemp.createTemp('reddb-dart-embedded-');
    final database = File('${directory.path}/driver-contract.rdb');
    final uri = database.uri.toString();
    try {
      final first = await connect(uri);
      await first.query('CREATE TABLE durable (id INTEGER, name TEXT)');
      await first.query(
        "INSERT INTO durable (id, name) VALUES (1, 'persisted')",
      );
      await expectLater(connect(uri), throwsA(isA<RedDBError>()));
      await first.close();

      final second = await connect(uri);
      try {
        final rows =
            await second.query('SELECT name FROM durable WHERE id = 1');
        expect(utf8.decode(rows), contains('persisted'));
      } finally {
        await second.close();
      }

      expect(directory.listSync().map((entry) => entry.path).toList(),
          [database.path]);
    } finally {
      await directory.delete(recursive: true);
    }
  });
}
