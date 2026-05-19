import 'dart:convert';
import 'dart:typed_data';

import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

class _FakeCall {
  _FakeCall(this.sql, this.params);
  final String sql;
  final List<Object?>? params;
}

class _FakeQuerier implements Querier {
  _FakeQuerier({List<Object?>? replies, List<Object?>? errors})
      : _replies = List.from(replies ?? const []),
        _errors = List.from(errors ?? const []);

  final List<_FakeCall> calls = [];
  final List<Object?> _replies;
  final List<Object?> _errors;

  @override
  Future<Uint8List> query(String sql, {List<Object?>? params}) async {
    calls.add(_FakeCall(sql, params));
    final err = _errors.isEmpty ? null : _errors.removeAt(0);
    if (err != null) throw err;
    final reply = _replies.isEmpty ? null : _replies.removeAt(0);
    return Uint8List.fromList(utf8.encode(jsonEncode(reply ?? {})));
  }
}

Uint8List _reply(Object? v) =>
    Uint8List.fromList(utf8.encode(jsonEncode(v)));

void main() {
  group('kvPath', () {
    test('quotes namespaced keys', () {
      expect(kvPath('kv_default', 'corpus:version'),
          "kv_default.'corpus:version'");
    });
    test('preserves dots and slashes inside the key', () {
      expect(kvPath('kv_default', 'a/b.c'), "kv_default.'a/b.c'");
    });
    test('rejects bad collection', () {
      expect(
        () => kvPath('bad-name!', 'k'),
        throwsA(isA<InvalidArgument>()),
      );
    });
  });

  group('KV', () {
    test('set emits exact key path and value literal', () async {
      final q = _FakeQuerier(replies: [<String, Object?>{}]);
      await Helpers(q).kv.set('characters:hansel', 'ok');
      final sql = q.calls.first.sql;
      expect(sql, contains("kv_default.'characters:hansel'"));
      expect(sql, contains("= 'ok'"));
    });

    test('escapes single quotes in value literal', () async {
      final q = _FakeQuerier(replies: [<String, Object?>{}]);
      await Helpers(q).kv.set('k', "o'reilly");
      expect(q.calls.first.sql, contains("= 'o''reilly'"));
    });

    test('get returns value or null on miss', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'value': 'v'},
          ],
        },
        {'rows': <Object?>[]},
      ]);
      final kv = Helpers(q).kv;
      expect(await kv.get('k'), 'v');
      expect(await kv.get('k2'), isNull);
    });

    test('exists is true when get returns a value', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'value': 'v'},
          ],
        },
        {'rows': <Object?>[]},
      ]);
      final kv = Helpers(q).kv;
      expect((await kv.exists('k')).exists, isTrue);
      expect((await kv.exists('k2')).exists, isFalse);
    });

    test('list filters by prefix without rewriting the SQL', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'key': 'a:1', 'value': 1},
            {'key': 'b:1', 'value': 2},
            {'key': 'a:2', 'value': 3},
          ],
        },
      ]);
      final res = await Helpers(q).kv.list(prefix: 'a:');
      expect(res.items, hasLength(2));
      expect(res.items.map((r) => r['key']), ['a:1', 'a:2']);
      expect(q.calls.first.sql, isNot(contains('a:')));
    });

    test('list rejects negative limit', () async {
      expect(
        () => Helpers(_FakeQuerier()).kv.list(limit: -1),
        throwsA(isA<InvalidArgument>()),
      );
    });
  });

  group('Queue', () {
    test('push emits priority and JSON payload', () async {
      final q = _FakeQuerier(replies: [
        {'affected': 1},
      ]);
      await Helpers(q).queue.push('jobs', {'id': 1}, priority: 5);
      final sql = q.calls.first.sql;
      expect(sql, startsWith('QUEUE PUSH jobs '));
      expect(sql, contains('PRIORITY 5'));
      expect(sql, contains('{"id":1}'));
    });

    test('len returns int from row', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'len': 3},
          ],
        },
      ]);
      expect(await Helpers(q).queue.len('jobs'), 3);
    });

    test('pop returns payload list', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'payload': 'a'},
            {'payload': 'b'},
          ],
        },
      ]);
      final out = await Helpers(q).queue.pop('jobs', count: 2);
      expect(out, ['a', 'b']);
    });

    test('pop rejects negative count', () async {
      expect(
        () => Helpers(_FakeQuerier()).queue.pop('jobs', count: -1),
        throwsA(isA<InvalidArgument>()),
      );
    });

    test('push rejects invalid identifier', () async {
      expect(
        () => Helpers(_FakeQuerier()).queue.push('bad-name!', 'x'),
        throwsA(isA<InvalidArgument>()),
      );
    });
  });

  group('Documents', () {
    test('insert returns rid envelope', () async {
      final q = _FakeQuerier(replies: [
        {'rows': <Object?>[], 'affected': 0},
        {
          'rows': [
            {
              'rid': 'doc-1',
              'body': {'name': 'alice'},
            },
          ],
          'affected': 1,
        },
      ]);
      final out =
          await Helpers(q).documents.insert('people', {'name': 'alice'});
      expect(out.affected, 1);
      expect(out.rid, 'doc-1');
      expect(out.item!['rid'], 'doc-1');
    });

    test('get raises NotFound on missing row', () async {
      final q = _FakeQuerier(replies: [
        {'rows': <Object?>[]},
      ]);
      expect(
        () => Helpers(q).documents.get('people', 'doc-1'),
        throwsA(isA<NotFound>()),
      );
    });

    test('patch rejects JSON-pointer-style paths', () async {
      expect(
        () => Helpers(_FakeQuerier())
            .documents
            .patch('people', 'doc-1', {'a/b': 1}),
        throwsA(isA<InvalidArgument>()),
      );
    });

    test('list orders by rid ASC by default', () async {
      final q = _FakeQuerier(replies: [
        {
          'rows': [
            {'rid': 'a'},
            {'rid': 'b'},
          ],
        },
      ]);
      final out = await Helpers(q).documents.list('people');
      expect(out.items, hasLength(2));
      expect(q.calls.first.sql, contains('ORDER BY rid ASC'));
    });

    test('passes through "collection already exists" error', () async {
      final q = _FakeQuerier(
        replies: [
          <String, Object?>{},
          {
            'rows': [
              {'rid': 'x'},
            ],
            'affected': 1,
          },
        ],
        errors: [Exception('collection already exists'), null],
      );
      final out = await Helpers(q).documents.insert('people', {'a': 1});
      expect(out.rid, 'x');
    });
  });

  group('response parsing', () {
    test('nested result.affected envelope is honoured by delete', () async {
      final q = _FakeQuerier(replies: [
        {
          'result': {'affected': 7},
        },
      ]);
      final out = await Helpers(q).queue.purge('jobs');
      expect(out.affected, 7);
      expect(out.deleted, isTrue);
    });
  });

  // --- SDK Helper Spec v1.0 ---------------------------------------------

  group('spec v1.0', () {
    test('exposes HELPER_SPEC_VERSION = "1.0"', () {
      expect(Helpers.helperSpecVersion, '1.0');
    });

    test('documents.patch rejects empty patch with INVALID_ARGUMENT', () async {
      final q = _FakeQuerier();
      expect(
        () => Helpers(q).documents.patch('people', 'doc-1', {}),
        throwsA(isA<InvalidArgument>()),
      );
      expect(q.calls, isEmpty,
          reason: 'empty patch must reject before issuing any query');
    });

    test('documents.delete of missing rid returns deleted=false (not error)',
        () async {
      final q = _FakeQuerier(replies: [
        {'affected': 0},
      ]);
      final out = await Helpers(q).documents.delete('people', 'no-such-rid');
      expect(out.affected, 0);
      expect(out.deleted, isFalse);
    });

    test('documents.delete of present rid returns deleted=true', () async {
      final q = _FakeQuerier(replies: [
        {'affected': 1},
      ]);
      final out = await Helpers(q).documents.delete('people', 'doc-1');
      expect(out.affected, 1);
      expect(out.deleted, isTrue);
    });

    test('kv.delete of missing key returns deleted=false', () async {
      final q = _FakeQuerier(replies: [
        {'affected': 0},
      ]);
      final out = await Helpers(q).kv.delete('nope');
      expect(out.affected, 0);
      expect(out.deleted, isFalse);
    });

    test('documents.bulkInsert empty is no-op', () async {
      final q = _FakeQuerier();
      final out = await Helpers(q).documents.bulkInsert('people', const []);
      expect(out.affected, 0);
      expect(out.rids, isEmpty);
      expect(q.calls, isEmpty);
    });

    test('documents.bulkInsert preserves per-row rid order', () async {
      final q = _FakeQuerier(replies: [
        // _ensureCollection
        <String, Object?>{},
        // first insert
        {
          'rows': [
            {'rid': 'r-1'},
          ],
          'affected': 1,
        },
        // second insert
        {
          'rows': [
            {'rid': 'r-2'},
          ],
          'affected': 1,
        },
      ]);
      final out = await Helpers(q).documents.bulkInsert('people', [
        {'i': 1},
        {'i': 2},
      ]);
      expect(out.affected, 2);
      expect(out.rids, ['r-1', 'r-2']);
    });

    test('queues alias and queues.create emit CREATE QUEUE IF NOT EXISTS',
        () async {
      final q = _FakeQuerier(replies: [<String, Object?>{}]);
      await Helpers(q).queues.create('jobs');
      expect(q.calls.first.sql, 'CREATE QUEUE IF NOT EXISTS jobs');
    });

    test('queues.create rejects bad identifier', () async {
      expect(
        () => Helpers(_FakeQuerier()).queues.create('bad-name!'),
        throwsA(isA<InvalidArgument>()),
      );
    });

    test('tx.begin / commit / rollback emit BEGIN / COMMIT / ROLLBACK',
        () async {
      final q = _FakeQuerier(
          replies: [<String, Object?>{}, <String, Object?>{}, <String, Object?>{}]);
      final tx = Helpers(q).tx();
      await tx.begin();
      await tx.commit();
      await tx.begin();
      await tx.rollback();
      expect(q.calls.map((c) => c.sql), ['BEGIN', 'COMMIT', 'BEGIN', 'ROLLBACK']);
    });

    test('tx.run commits on success', () async {
      final q = _FakeQuerier(replies: [
        <String, Object?>{}, // BEGIN
        <String, Object?>{}, // body insert
        <String, Object?>{}, // COMMIT
      ]);
      final tx = Helpers(q).tx();
      await tx.run((t) async {
        await q.query('SOMETHING');
      });
      expect(q.calls.map((c) => c.sql), ['BEGIN', 'SOMETHING', 'COMMIT']);
    });

    test('tx.run rolls back and re-throws on error', () async {
      final q = _FakeQuerier(
        replies: [<String, Object?>{}, <String, Object?>{}, <String, Object?>{}],
        errors: [null, Exception('boom'), null],
      );
      final tx = Helpers(q).tx();
      await expectLater(
        tx.run((t) async => q.query('SOMETHING')),
        throwsA(isA<Exception>()),
      );
      expect(q.calls.map((c) => c.sql), ['BEGIN', 'SOMETHING', 'ROLLBACK']);
    });

    test('tx.run rejects nested run with INVALID_ARGUMENT', () async {
      final q = _FakeQuerier(replies: [<String, Object?>{}]);
      final tx = Helpers(q).tx();
      await expectLater(
        tx.run((outer) async {
          await outer.run((_) async {});
        }),
        throwsA(isA<InvalidArgument>()),
      );
    });
  });
}
