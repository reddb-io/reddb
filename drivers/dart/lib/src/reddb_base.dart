import 'dart:typed_data';

import 'conn.dart';
import 'embedded/embedded_conn_stub.dart'
    if (dart.library.ffi) 'embedded/embedded_conn_native.dart';
import 'errors.dart';
import 'helpers.dart';
import 'http/client.dart';
import 'options.dart';
import 'redwire/conn.dart';
import 'url.dart';
import 'routing.dart';

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
  if (parsed.isEmbedded) return _connectDirect(uri, options: options);
  final protocol = parsed.isRedwire ? (parsed.isTls || options.tls != null ? 'reds' : 'red') : parsed.kind;
  final host = parsed.host?.contains(':') == true ? '[${parsed.host}]' : parsed.host;
  final seed = topologyAddress('$protocol://$host:${parsed.port ?? defaultPortFor(parsed.kind)}${parsed.path ?? ""}', protocol);
  if (seed == null) throw InvalidUri('invalid remote endpoint');
  final original = Uri.parse(uri);
  Future<Conn> dial(String address) async {
    var target = address;
    if (original.userInfo.isNotEmpty) target = target.replaceFirst('://', '://${original.userInfo}@');
    if (original.hasQuery) target += '?${original.query}';
    return (await _connectDirect(target, options: options))._conn;
  }
  final router = RoutedConn(seed, (await _connectDirect(uri, options: options))._conn,
      protocol, parsed.params['route']?.toLowerCase() == 'primary', dial);
  try { await router.refresh(); return Reddb._(router); }
  catch (_) { await router.close(); rethrow; }
}

Future<Reddb> _connectDirect(
  String uri, {
  ConnectOptions options = const ConnectOptions(),
}) async {
  final parsed = parseUri(uri);
  if (parsed.isEmbedded) {
    return Reddb._(await openEmbedded(parsed));
  }
  if (parsed.host == null) {
    throw InvalidUri("URI '$uri' is missing a host");
  }

  if (parsed.isHttp) {
    final scheme = parsed.kind; // 'http' or 'https'
    final port = parsed.port ?? defaultPortFor(parsed.kind);
    final host = parsed.host!.contains(':') ? '[${parsed.host}]' : parsed.host;
    final base = '$scheme://$host:$port${parsed.path ?? ""}';
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
class Reddb implements Querier {
  Reddb._(this._conn);

  final Conn _conn;

  Future<Uint8List> queryWithOptions(String sql, ReadOptions options, {List<Object?>? params}) {
    final client = _conn;
    if (client is RoutedConn) return client.queryWithOptions(sql, options, params: params);
    throw RedDBError('CONSISTENCY_UNSUPPORTED', 'connection does not support read consistency');
  }

  /// Rich SDK Helper namespaces (`documents`, `kv`, `queue`).
  /// See `docs/clients/sdk-helper-spec.md`.
  late final Helpers helpers = Helpers(this);

  /// Run a SQL string with optional positional params.
  ///
  /// Returns the raw response bytes (UTF-8 JSON).
  @override
  Future<Uint8List> query(String sql, {List<Object?>? params}) =>
      _conn.query(sql, params: params);

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
