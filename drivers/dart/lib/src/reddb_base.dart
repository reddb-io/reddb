import 'dart:typed_data';

import 'conn.dart';
import 'errors.dart';
import 'http/client.dart';
import 'options.dart';
import 'redwire/conn.dart';
import 'url.dart';

/// Top-level entry point. Pass any RedDB connection URI; the right
/// transport is picked up automatically.
///
/// ```dart
/// final db = await connect('red://localhost:5050');
/// final res = await db.query('SELECT 1');
/// await db.close();
/// ```
Future<Reddb> connect(
  String uri, {
  ConnectOptions options = const ConnectOptions(),
}) async {
  final parsed = parseUri(uri);
  if (parsed.isEmbedded) {
    throw EmbeddedUnsupported();
  }
  if (parsed.host == null) {
    throw InvalidUri("URI '$uri' is missing a host");
  }

  if (parsed.isHttp) {
    final scheme = parsed.kind; // 'http' or 'https'
    final port = parsed.port ?? defaultPortFor(parsed.kind);
    final base = '$scheme://${parsed.host}:$port';
    final http = HttpConn(
      baseUrl: base,
      token: options.token ?? parsed.token,
      timeout: options.timeout,
    );
    if ((options.token ?? parsed.token) == null) {
      final user = options.username ?? parsed.username;
      final pass = options.password ?? parsed.password;
      if (user != null && pass != null) {
        await http.login(user, pass);
      }
    }
    return Reddb._(http);
  }

  // RedWire: plaintext or TLS.
  final tls = parsed.kind == 'redwire-tls' || options.tls != null;
  final port = parsed.port ?? defaultPortFor(parsed.kind);
  final token = options.token ?? parsed.token;
  final conn = await RedwireConn.connect(
    host: parsed.host!,
    port: port,
    token: token,
    clientName: options.clientName,
    tls: tls,
    tlsOpts: options.tls,
    timeout: options.timeout,
  );
  return Reddb._(conn);
}

/// Public façade returned by `connect()`. Thin wrapper around the
/// underlying [Conn]; methods return raw JSON bytes for `query`/`get`,
/// matching the JS / Python drivers' "decode at the call site" stance.
class Reddb {
  Reddb._(this._conn);

  final Conn _conn;

  /// Run a SQL string. Returns the raw response bytes (UTF-8 JSON).
  Future<Uint8List> query(String sql) => _conn.query(sql);

  /// Insert a single row. `payload` is a JSON-encodable map.
  Future<Uint8List> insert(String collection, Map<String, dynamic> payload) =>
      _conn.insert(collection, payload);

  /// Bulk-insert rows.
  Future<Uint8List> bulkInsert(
    String collection,
    List<Map<String, dynamic>> rows,
  ) =>
      _conn.bulkInsert(collection, rows);

  /// Get one row by primary id.
  Future<Uint8List> get(String collection, String id) =>
      _conn.get(collection, id);

  /// Delete one row by primary id.
  Future<Uint8List> delete(String collection, String id) =>
      _conn.delete(collection, id);

  /// Liveness check.
  Future<void> ping() => _conn.ping();

  /// Best-effort clean shutdown. Idempotent.
  Future<void> close() => _conn.close();
}
