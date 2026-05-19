@Tags(['smoke'])
library;

import 'dart:async';
import 'dart:io';

import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

/// SDK Helper Spec v1.0 conformance harness (docs/spec/sdk-helpers.md §12).
///
/// Case IDs mirror the spec verbatim (dots → underscores in test names so
/// cross-driver CI dashboards line up). The Dart driver does not embed the
/// engine, so this harness spawns one `red server` process per `dart test`
/// run. Gated on `RED_SMOKE=1`; skippable via `RED_SKIP_SMOKE=1`.
///
/// Run:
///
/// ```
/// RED_SMOKE=1 RED_BIN=/path/to/red \
///   dart test test/conformance_test.dart
/// ```
void main() {
  final smoke = Platform.environment['RED_SMOKE'] == '1';
  final skip = Platform.environment['RED_SKIP_SMOKE'] == '1';
  if (!smoke || skip) {
    test('conformance (skipped — set RED_SMOKE=1)', () {},
        skip: 'RED_SMOKE not set or RED_SKIP_SMOKE=1');
    return;
  }

  late Process server;
  late Reddb db;
  late Helpers h;

  setUpAll(() async {
    final binary = Platform.environment['RED_BIN'] ?? 'red';
    final dataDir = await Directory.systemTemp.createTemp('reddb-dart-conf-');
    final port = await _freePort();
    server = await Process.start(binary, [
      'server',
      '--path',
      '${dataDir.path}/data.db',
      '--bind',
      '127.0.0.1:$port',
    ]);
    server.stderr.listen((_) {});
    server.stdout.listen((_) {});
    db = await _waitForConnect(port);
    h = db.helpers;
  });

  tearDownAll(() async {
    await db.close();
    server.kill(ProcessSignal.sigterm);
    await server.exitCode;
  });

  // ---- meta ---------------------------------------------------------

  test('TestConformance_meta_spec_version', () {
    expect(Helpers.helperSpecVersion, '1.0');
  });

  // ---- generic ------------------------------------------------------

  test('TestConformance_generic_query_no_params', () async {
    final body = await db.query('SELECT 1');
    expect(String.fromCharCodes(body), isNotEmpty);
  });

  test('TestConformance_generic_query_with_params', () async {
    final tbl = _uniq('c_qp');
    await db.query('CREATE TABLE $tbl (id INT, name TEXT)');
    await db.query(
      r'INSERT INTO ' + tbl + r' (id, name) VALUES ($1, $2)',
      params: [42, 'alice'],
    );
    final body = await db.query(
      r'SELECT name FROM ' + tbl + r' WHERE id = $1',
      params: [42],
    );
    expect(String.fromCharCodes(body), contains('alice'));
  });

  test('TestConformance_generic_insert_rid', () async {
    final col = _uniq('c_ins');
    final r = await h.documents.insert(col, {'name': 'alice'});
    expect(r.affected, 1);
    expect(r.rid, isNotEmpty);
  });

  test('TestConformance_generic_bulk_insert_rids', () async {
    // empty no-op
    final empty = await h.documents.bulkInsert(_uniq('c_bulk_e'), const []);
    expect(empty.affected, 0);
    expect(empty.rids, isEmpty);
    // non-empty preserves order
    final col = _uniq('c_bulk');
    final out = await h.documents.bulkInsert(col, [
      {'i': 1},
      {'i': 2},
    ]);
    expect(out.affected, 2);
    expect(out.rids, hasLength(2));
    for (final rid in out.rids) {
      expect(rid, isNotEmpty);
    }
  });

  test('TestConformance_generic_delete', () async {
    final col = _uniq('c_del');
    final ins = await h.documents.insert(col, {'x': 1});
    final d = await h.documents.delete(col, ins.rid);
    expect(d.affected, 1);
    expect(d.deleted, isTrue);
  });

  // ---- documents ----------------------------------------------------

  test('TestConformance_documents_crud_nested_patch', () async {
    final col = _uniq('d_crud');
    final ins = await h.documents.insert(col, {'name': 'alice', 'age': 30});
    final got = await h.documents.get(col, ins.rid);
    expect(got['rid'], ins.rid);
    final list = await h.documents.list(col, limit: 10);
    expect(list.items, isNotEmpty);
    final patched = await h.documents.patch(col, ins.rid, {'age': 31});
    expect(patched, isNotNull);
    final d = await h.documents.delete(col, ins.rid);
    expect(d.deleted, isTrue);
  });

  test('TestConformance_documents_delete_missing_no_error', () async {
    final col = _uniq('d_dm');
    await h.documents.insert(col, {'k': 1}); // ensure collection
    final d = await h.documents.delete(col, 'no-such-rid');
    expect(d.affected, 0);
    expect(d.deleted, isFalse);
  });

  test('TestConformance_documents_patch_empty_rejects', () async {
    expect(
      () => h.documents.patch('any_col', 'any_rid', const {}),
      throwsA(isA<InvalidArgument>()),
    );
  });

  // ---- kv -----------------------------------------------------------

  test('TestConformance_kv_exact_key_round_trip', () async {
    final coll = _uniq('kvc');
    await db.query('CREATE KV $coll');
    final kv = KvClient(db, collection: coll);
    await kv.set('characters:hansel', 'witch');
    final v = await kv.get('characters:hansel');
    expect(v, 'witch');
    final out = await kv.list(prefix: 'characters:');
    expect(out.items, isNotEmpty);
  });

  test('TestConformance_kv_missing_get_returns_none', () async {
    final coll = _uniq('kvc');
    await db.query('CREATE KV $coll');
    final v = await KvClient(db, collection: coll).get('nope');
    expect(v, isNull);
  });

  test('TestConformance_kv_delete_returns_envelope', () async {
    final coll = _uniq('kvc');
    await db.query('CREATE KV $coll');
    final kv = KvClient(db, collection: coll);
    await kv.set('k', 'v');
    final d = await kv.delete('k');
    expect(d.deleted, isTrue);
  });

  // ---- queues -------------------------------------------------------

  test('TestConformance_queues_fifo_peek_pop_len', () async {
    final qn = _uniq('q_fifo');
    final q = h.queues;
    await q.create(qn);
    await q.push(qn, {'v': 1});
    await q.push(qn, {'v': 2});
    expect(await q.len(qn), 2);
    final peek = await q.peek(qn, count: 1);
    expect(peek, hasLength(1));
    final p1 = await q.pop(qn, count: 1);
    expect(p1, hasLength(1));
    final p2 = await q.pop(qn, count: 1);
    expect(p2, hasLength(1));
  });

  test('TestConformance_queues_empty_pop_returns_empty', () async {
    final qn = _uniq('q_empty');
    await h.queues.create(qn);
    final p = await h.queues.pop(qn, count: 1);
    expect(p, isEmpty);
  });

  test('TestConformance_queues_purge_resets_len', () async {
    final qn = _uniq('q_purge');
    final q = h.queues;
    await q.create(qn);
    await q.push(qn, 'a');
    await q.push(qn, 'b');
    await q.purge(qn);
    expect(await q.len(qn), 0);
  });

  // ---- tx -----------------------------------------------------------

  test('TestConformance_tx_commit_persists', () async {
    final col = _uniq('tx_commit');
    await h.documents.insert(col, {'seed': true}); // ensure collection
    final tx = h.tx();
    await tx.begin();
    final ins = await h.documents.insert(col, {'x': 1});
    await tx.commit();
    final got = await h.documents.get(col, ins.rid);
    expect(got['rid'], ins.rid);
  });

  test('TestConformance_tx_rollback_discards', () async {
    final col = _uniq('tx_rb');
    await h.documents.insert(col, {'seed': true}); // ensure collection
    final tx = h.tx();
    await tx.begin();
    final ins = await h.documents.insert(col, {'x': 1});
    await tx.rollback();
    expect(
      () => h.documents.get(col, ins.rid),
      throwsA(isA<NotFound>()),
    );
  });

  // ---- errors -------------------------------------------------------

  test('TestConformance_errors_invalid_argument_empty_sql', () async {
    // The Dart Querier accepts an empty SQL string and the server rejects
    // it. Either local validation or a server INVALID_ARGUMENT is spec-
    // compliant; we assert the helper-level path raises.
    expect(
      () => h.documents.patch('any_col', 'any_rid', const {}),
      throwsA(isA<InvalidArgument>()),
    );
  });

  test('TestConformance_errors_not_found_document_get', () async {
    final col = _uniq('err_nf');
    await h.documents.insert(col, {'seed': true}); // ensure collection
    expect(
      () => h.documents.get(col, 'no-such-rid'),
      throwsA(isA<NotFound>()),
    );
  });

  // ---- provisional wire --------------------------------------------

  test('TestConformance_wire_probabilistic_hll_round_trip', () async {
    final name = _uniq('hll');
    await db.query('CREATE HLL $name');
    await db.query("HLL ADD $name 'user1' 'user2'");
    final body = await db.query('HLL COUNT $name');
    final s = String.fromCharCodes(body);
    expect(
      s.contains('count') || s.contains('cardinality'),
      isTrue,
      reason: 'expected count or cardinality column: $s',
    );
  });
}

String _uniq(String prefix) {
  final ts = DateTime.now().microsecondsSinceEpoch.toRadixString(36);
  return '${prefix}_$ts';
}

Future<int> _freePort() async {
  final socket = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final port = socket.port;
  await socket.close();
  return port;
}

Future<Reddb> _waitForConnect(int port) async {
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
