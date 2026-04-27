import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:http/http.dart' as http;

import '../conn.dart';
import '../errors.dart';

/// HTTP transport. Stateless: every request carries an `Authorization:
/// Bearer <token>` header (when a token is set). `setToken()` swaps it
/// in-place — used by the auto-login flow.
class HttpConn implements Conn {
  HttpConn({
    required this.baseUrl,
    String? token,
    http.Client? client,
    this.timeout = const Duration(seconds: 30),
  })  : _token = token,
        _client = client ?? http.Client();

  final String baseUrl;
  final http.Client _client;
  final Duration timeout;
  String? _token;
  bool _closed = false;

  String? get token => _token;
  void setToken(String? token) {
    _token = token;
  }

  /// Trade `username` / `password` for a bearer token via
  /// `POST /auth/login`. Stores the token on the client when
  /// successful.
  Future<void> login(String username, String password) async {
    final body = await _request(
      'POST',
      '/auth/login',
      body: jsonEncode({'username': username, 'password': password}),
    );
    final parsed = _decodeJson(body);
    if (parsed is Map && parsed['token'] is String) {
      setToken(parsed['token'] as String);
    } else if (parsed is Map &&
        parsed['result'] is Map &&
        (parsed['result'] as Map)['token'] is String) {
      setToken((parsed['result'] as Map)['token'] as String);
    } else {
      throw AuthRefused('login response missing token');
    }
  }

  // ---------------------------------------------------------------------
  // Conn surface
  // ---------------------------------------------------------------------

  @override
  Future<Uint8List> query(String sql) async {
    final resp = await _request(
      'POST',
      '/query',
      body: jsonEncode({'query': sql}),
    );
    return resp;
  }

  @override
  Future<Uint8List> insert(
    String collection,
    Map<String, dynamic> payload,
  ) async {
    final path =
        '/collections/${Uri.encodeComponent(collection)}/rows';
    return _request('POST', path, body: jsonEncode(payload));
  }

  @override
  Future<Uint8List> bulkInsert(
    String collection,
    List<Map<String, dynamic>> rows,
  ) async {
    final path =
        '/collections/${Uri.encodeComponent(collection)}/bulk/rows';
    return _request('POST', path, body: jsonEncode({'rows': rows}));
  }

  @override
  Future<Uint8List> get(String collection, String id) async {
    final path =
        '/collections/${Uri.encodeComponent(collection)}/${Uri.encodeComponent(id)}';
    return _request('GET', path);
  }

  @override
  Future<Uint8List> delete(String collection, String id) async {
    final path =
        '/collections/${Uri.encodeComponent(collection)}/${Uri.encodeComponent(id)}';
    return _request('DELETE', path);
  }

  @override
  Future<void> ping() async {
    await _request('GET', '/admin/health');
  }

  @override
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _client.close();
  }

  // ---------------------------------------------------------------------
  // Internals
  // ---------------------------------------------------------------------

  Future<Uint8List> _request(
    String method,
    String path, {
    String? body,
  }) async {
    final uri = Uri.parse('${_baseTrimmed()}$path');
    final headers = <String, String>{};
    final tk = _token;
    if (tk != null && tk.isNotEmpty) {
      headers['authorization'] = 'Bearer $tk';
    }
    if (body != null) {
      headers['content-type'] = 'application/json';
    }
    final req = http.Request(method, uri);
    req.headers.addAll(headers);
    if (body != null) req.body = body;

    final streamed = await _client.send(req).timeout(timeout);
    final bytes = await streamed.stream.toBytes();

    if (streamed.statusCode < 200 || streamed.statusCode >= 300) {
      final parsed = _decodeJson(bytes);
      String code = 'HTTP_${streamed.statusCode}';
      String message = 'request failed with status ${streamed.statusCode}';
      if (parsed is Map) {
        if (parsed['error_code'] is String) {
          code = parsed['error_code'] as String;
        }
        if (parsed['error'] is String) {
          message = parsed['error'] as String;
        } else if (parsed['message'] is String) {
          message = parsed['message'] as String;
        }
      }
      if (streamed.statusCode == 401 || streamed.statusCode == 403) {
        throw AuthRefused(message, parsed);
      }
      throw RedDBError(code, message, parsed);
    }
    return bytes;
  }

  String _baseTrimmed() =>
      baseUrl.endsWith('/') ? baseUrl.substring(0, baseUrl.length - 1) : baseUrl;

  static Object? _decodeJson(Uint8List bytes) {
    if (bytes.isEmpty) return null;
    try {
      return jsonDecode(utf8.decode(bytes));
    } catch (_) {
      return null;
    }
  }
}
