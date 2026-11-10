import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'package:test/test.dart';
import 'package:reddb/src/routing.dart';
import 'package:reddb/src/conn.dart';

class Peer implements Conn, TopologySource {
  Peer(this.address, this.calls, this.membership);
  final String address;
  final List<(String, String)> calls;
  final Map<String, dynamic> membership;
  @override
  Future<Map<String, dynamic>?> topology(String protocol) async => membership;
  @override
  Future<Uint8List> query(String sql, {List<Object?>? params}) async {
    calls.add((address, sql));
    if (sql.contains('lost') || sql.contains('effectful')) {
      throw const SocketException('lost reply');
    }
    return Uint8List.fromList(utf8.encode(address));
  }

  @override
  Future<void> ping() async {}
  @override
  Future<void> close() async {}
  @override
  Future<Uint8List> insert(String c, Map<String, dynamic> p) => query('insert');
  @override
  Future<Uint8List> bulkInsert(String c, List<Map<String, dynamic>> p) =>
      query('bulk');
  @override
  Future<Uint8List> get(String c, String id) => query('get');
  @override
  Future<Uint8List> delete(String c, String id) => query('delete');
}

void main() {
  final fixtures = Directory('../../testdata/conformance/topology');
  test('canonical topology and SQL fixtures', () {
    final manifest =
        jsonDecode(File('${fixtures.path}/manifest.json').readAsStringSync()) as Map<String, dynamic>;
    for (final fixture in (manifest['cases'] as List<dynamic>).cast<Map<String, dynamic>>()) {
      final hex = fixture['hex'] as String;
      final bytes = Uint8List.fromList([
        for (var i = 0; i < hex.length; i += 2)
          int.parse(hex.substring(i, i + 2), radix: 16)
      ]);
      if (fixture['error'] == true) {
        expect(() => decodeTopology(bytes), throwsA(isA<Exception>()));
      } else {
        expect(decodeTopology(bytes), equals(fixture['expected']),
            reason: fixture['name'] as String);
      }
    }
    for (final fixture in (jsonDecode(
        File('${fixtures.path}/sql-policy.json').readAsStringSync()) as List<dynamic>).cast<Map<String, dynamic>>()) {
      expect(sqlRoutingPolicy(fixture['sql'] as String).$2, fixture['replica_read'],
          reason: fixture['sql'] as String);
    }
  });
  test('routes reads, pins transactions and never replays effects', () async {
    const primary = 'https://primary:5000', replica = 'https://replica:5000';
    final calls = <(String, String)>[];
    final membership = <String, dynamic>{
      'version': 1,
      'primary': {'addr': primary},
      'replicas': [
        {'addr': replica, 'healthy': true, 'lag_ms': 10}
      ]
    };
    Future<Conn> dial(String address) async => Peer(address, calls, membership);
    final client =
        RoutedConn(primary, await dial(primary), 'https', false, dial);
    expect(utf8.decode(await client.query('SELECT 1')), primary);
    expect(
        utf8.decode(await client.queryWithOptions(
            'SELECT 1', const ReadOptions(ReadConsistency.local))),
        replica);
    await client.query('BEGIN');
    expect(
        utf8.decode(await client.queryWithOptions(
            'SELECT 1', const ReadOptions(ReadConsistency.local))),
        primary);
    await client.query('ROLLBACK');
    for (final sql in ['UPDATE lost', 'SELECT "effectful"()']) {
      final before = calls.length;
      await expectLater(
          client.queryWithOptions(
              sql, const ReadOptions(ReadConsistency.local)),
          throwsA(isA<SocketException>()));
      expect(calls.length, before + 1);
      expect(calls.last.$1, primary);
    }
    await client.close();
  });
}
