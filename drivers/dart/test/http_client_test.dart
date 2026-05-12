import 'dart:convert';
import 'dart:typed_data';

import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:reddb/src/http/client.dart';
import 'package:test/test.dart';

void main() {
  group('HttpConn query params', () {
    test('query without params keeps legacy body shape', () async {
      late Map<String, Object?> body;
      final conn = HttpConn(
        baseUrl: 'http://127.0.0.1:8080',
        client: MockClient((request) async {
          body = jsonDecode(request.body) as Map<String, Object?>;
          return http.Response.bytes(utf8.encode('{}'), 200);
        }),
      );

      await conn.query('SELECT 1');
      expect(body, {'query': 'SELECT 1'});
    });

    test('query with params sends typed HTTP params', () async {
      late Map<String, Object?> body;
      final conn = HttpConn(
        baseUrl: 'http://127.0.0.1:8080',
        client: MockClient((request) async {
          body = jsonDecode(request.body) as Map<String, Object?>;
          return http.Response.bytes(utf8.encode('{}'), 200);
        }),
      );

      await conn.query(r'SELECT $1, $2, $3, $4', [
        42,
        'alice',
        null,
        Float32List.fromList([1.0, 2.0]),
      ]);

      expect(body['query'], r'SELECT $1, $2, $3, $4');
      expect(body['params'], [
        42,
        'alice',
        null,
        [1.0, 2.0],
      ]);
    });
  });
}
