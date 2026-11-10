import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'package:http/http.dart' as http;
import 'conn.dart';
import 'errors.dart';

enum ReadConsistency { strong, local, boundedStaleness }

class ReadOptions {
  const ReadOptions(this.consistency, {this.maxLag = Duration.zero});
  final ReadConsistency consistency;
  final Duration maxLag;
}

abstract interface class TopologySource {
  Future<Map<String, dynamic>?> topology(String protocol);
}

// Authority: reddb-wire::topology. Shared fixtures pin every field and u64.
const topologyKind = 0x32;
const topologyFeature = 2;
Map<String, dynamic>? decodeTopology(Uint8List bytes) {
  if (bytes.length < 5) throw ProtocolError('truncated topology');
  final view = ByteData.sublistView(bytes);
  final end = 5 + view.getUint32(1, Endian.little);
  if (end > bytes.length) throw ProtocolError('truncated topology body');
  if (bytes[0] != 1) return null;
  var offset = 5;
  Uint8List take(int size) {
    if (size < 0 || size > end - offset) {
      throw ProtocolError('truncated topology field');
    }
    final start = offset;
    offset += size;
    return Uint8List.sublistView(bytes, start, offset);
  }

  int u32() => ByteData.sublistView(take(4)).getUint32(0, Endian.little);
  String u64() {
    final value = take(8);
    var integer = BigInt.zero;
    for (var index = 7; index >= 0; index--) {
      integer = (integer << 8) + BigInt.from(value[index]);
    }
    return integer.toString();
  }

  String text() {
    final size = u32();
    if (size > 4096) throw ProtocolError('topology string too long');
    try {
      return utf8.decode(take(size));
    } on FormatException {
      throw ProtocolError('invalid topology UTF-8');
    }
  }

  final epoch = u64();
  final primary = {'addr': text(), 'region': text()};
  final count = u32();
  if (count > 256) throw ProtocolError('too many topology replicas');
  final replicas = <Map<String, dynamic>>[];
  for (var index = 0; index < count; index++) {
    replicas.add({
      'addr': text(),
      'region': text(),
      'healthy': take(1)[0] != 0,
      'lag_ms': u32(),
      'last_applied_lsn': u64(),
      'rebootstrapping': false
    });
  }
  if (end - offset >= count)
    for (final replica in replicas) {
      replica['rebootstrapping'] = take(1)[0] != 0;
    }
  return {
    'version': 1,
    'epoch': epoch,
    'primary': primary,
    'replicas': replicas
  };
}

String? topologyAddress(Object? address, String protocol) {
  if (address is! String || address.isEmpty || address.length > 4096) {
    return null;
  }
  try {
    final uri =
        Uri.parse(address.contains('://') ? address : '$protocol://$address');
    if (uri.scheme != protocol ||
        uri.host.isEmpty ||
        uri.userInfo.isNotEmpty ||
        uri.hasQuery ||
        uri.hasFragment) {
      return null;
    }
    return uri.toString().replaceFirst(RegExp(r'/+$'), '');
  } on FormatException {
    return null;
  }
}

(String, bool) sqlRoutingPolicy(String sql) {
  final tokens = _tokens(sql);
  if (tokens == null || tokens.isEmpty || tokens.contains(';')) {
    return ('session', false);
  }
  final first = tokens.first;
  if (first == 'BEGIN' ||
      first == 'START' && tokens.length > 1 && tokens[1] == 'TRANSACTION') {
    return ('begin', false);
  }
  if (['COMMIT', 'END', 'ROLLBACK'].contains(first)) {
    if (first == 'ROLLBACK' && tokens.contains('TO')) return ('primary', false);
    return (
      tokens.contains('CHAIN') && !tokens.contains('NO') ? 'begin' : 'end',
      false
    );
  }
  if (['SET', 'RESET', 'USE', 'PREPARE', 'DEALLOCATE', 'DECLARE', 'LISTEN']
      .contains(first)) {
    return ('session', false);
  }
  final forbidden =
      'INSERT UPDATE DELETE CREATE ALTER DROP INTO FOR LOCK CALL EXECUTE SET QUEUE COPY [ ]'
          .split(' ');
  final functions =
      'COUNT SUM MIN MAX AVG LOWER UPPER LENGTH COALESCE ABS ROUND CAST EXISTS IN AS FROM WHERE AND OR NOT SELECT'
          .split(' ');
  var read = first == 'SELECT' && !tokens.any(forbidden.contains);
  for (var index = 0; index + 1 < tokens.length; index++) {
    if (tokens[index + 1] == '(' &&
        (tokens[index] == '#identifier' ||
            RegExp(r'^[A-Z_]').hasMatch(tokens[index])) &&
        !functions.contains(tokens[index])) {
      read = false;
    }
  }
  return ('primary', read);
}

List<String>? _tokens(String sql) {
  if (sql.length > 1048576) return null;
  final tokens = <String>[];
  var index = 0;
  while (index < sql.length) {
    final char = sql[index];
    if (char.trim().isEmpty) {
      index++;
      continue;
    }
    if (sql.startsWith('--', index)) {
      final end = sql.indexOf('\n', index + 2);
      index = end < 0 ? sql.length : end + 1;
      continue;
    }
    if (sql.startsWith('/*', index)) {
      var depth = 1;
      index += 2;
      while (index < sql.length && depth > 0) {
        if (sql.startsWith('/*', index)) {
          depth++;
          index += 2;
        } else if (sql.startsWith('*/', index)) {
          depth--;
          index += 2;
        } else
          index++;
        if (depth > 64) return null;
      }
      if (depth != 0) return null;
      continue;
    }
    if (char == "'" || char == '"' || char == '`') {
      index++;
      var closed = false;
      while (index < sql.length) {
        final value = sql[index++];
        if (value == r'\') return null;
        if (value == char) {
          if (index < sql.length && sql[index] == char) {
            index++;
            continue;
          }
          closed = true;
          break;
        }
      }
      if (!closed) return null;
      tokens.add(char == "'" ? '#value' : '#identifier');
      continue;
    }
    if (char.codeUnitAt(0) > 127) return null;
    final start = index++;
    if (RegExp(r'[A-Za-z_]').hasMatch(char)) {
      while (index < sql.length && RegExp(r'[A-Za-z_0-9]').hasMatch(sql[index])) {
        index++;
      }
    }
    tokens.add(sql.substring(start, index).toUpperCase());
  }
  if (tokens.isNotEmpty && tokens.last == ';') tokens.removeLast();
  return tokens;
}

class RoutedConn implements Conn {
  RoutedConn(
      this.seed, Conn initial, this.protocol, this.forcePrimary, this.dial)
      : primary = seed,
        clients = {seed: initial};
  final String seed, protocol;
  final bool forcePrimary;
  final Future<Conn> Function(String) dial;
  final Map<String, Conn> clients;
  String primary;
  List<Map<String, dynamic>> replicas = [];
  String? pinned;
  bool session = false, broken = false, closed = false;
  int next = 0;
  final Stopwatch clock = Stopwatch()..start();
  int refreshAt = 0;
  Future<void> _tail = Future.value();

  Future<T> _locked<T>(Future<T> Function() operation) async {
    final previous = _tail;
    final done = Completer<void>();
    _tail = done.future;
    await previous;
    try {
      return await operation();
    } finally {
      done.complete();
    }
  }

  Future<Conn> _dialBounded(String address) async {
    var abandoned = false;
    final pending = dial(address).then((client) async {
      if (abandoned || closed) {
        await client.close();
        throw RedDBError(
            'CONNECTION_CLOSED', 'connection closed while opening');
      }
      return client;
    });
    try {
      return await pending.timeout(const Duration(seconds: 5));
    } catch (_) {
      abandoned = true;
      rethrow;
    }
  }

  Future<Conn> _endpoint(String address) async {
    if (clients.containsKey(address)) return clients[address]!;
    final client = await _dialBounded(address);
    try {
      await client.ping().timeout(const Duration(seconds: 5));
    } catch (_) {
      await client.close();
      rethrow;
    }
    if (closed) {
      await client.close();
      throw RedDBError('CONNECTION_CLOSED', 'connection is closed');
    }
    return clients[address] = client;
  }

  Future<void> refresh() async {
    if (pinned != null || closed) return;
    refreshAt = clock.elapsedMilliseconds + 30000;
    final deadline = clock.elapsedMilliseconds + 5000;
    final candidates = <String>{
      primary,
      seed,
      ...replicas.map((r) => r['addr'] as String)
    }.toList();
    for (var index = 0;
        index < candidates.length &&
            index < 258 &&
            clock.elapsedMilliseconds < deadline;
        index++) {
      Conn? control;
      try {
        control = await _dialBounded(candidates[index]);
        if (control is! TopologySource) continue;
        final topology = await (control as TopologySource)
            .topology(protocol)
            .timeout(const Duration(seconds: 5));
        if (topology == null || topology['version'] != 1) continue;
        final writer =
            topologyAddress((topology['primary'] as Map?)?['addr'], protocol);
        if (writer == null) continue;
        if (!candidates.contains(writer)) candidates.add(writer);
        await _endpoint(writer);
        final ready = <Map<String, dynamic>>[];
        for (final value in (topology['replicas'] as List? ?? []).take(256)) {
          if (clock.elapsedMilliseconds >= deadline) break;
          final replica = value as Map<String, dynamic>;
          final address = topologyAddress(replica['addr'], protocol);
          if (address == null ||
              address == writer ||
              replica['healthy'] != true ||
              replica['rebootstrapping'] == true) {
            continue;
          }
          try {
            await _endpoint(address);
            ready.add({...replica, 'addr': address});
          } catch (_) {}
        }
        primary = writer;
        replicas = ready;
        final keep = {seed, writer, ...ready.map((r) => r['addr'])};
        for (final address in clients.keys.toList()) {
          if (!keep.contains(address)) await clients.remove(address)!.close();
        }
        if (writer == candidates[index]) return;
      } on AuthRefused {
        rethrow;
      } catch (_) {
      } finally {
        await control?.close();
      }
    }
  }

  bool _network(Object error) =>
      error is IOException ||
      error is TimeoutException ||
      error is http.ClientException ||
      error is RedDBError &&
          ['NETWORK', 'TIMEOUT', 'CONNECTION_CLOSED'].contains(error.code);
  Future<T> _invoke<T>(Future<T> Function(Conn) operation,
          {String? sql, bool read = false, ReadOptions? options}) =>
      _locked(() async {
        if (closed || broken) {
          throw RedDBError('CONNECTION_CLOSED', 'connection is closed');
        }
        if (options != null && options.maxLag.isNegative) {
          throw RedDBError('CONSISTENCY_UNSUPPORTED', 'invalid lag bound');
        }
        if (pinned == null && clock.elapsedMilliseconds >= refreshAt) {
          await refresh();
        }
        final policy = sql == null ? ('primary', false) : sqlRoutingPolicy(sql);
        final safeRead = read || options != null && policy.$2;
        final eligible = forcePrimary ||
                options?.consistency == ReadConsistency.strong
            ? <Map<String, dynamic>>[]
            : replicas
                .where((r) =>
                    options?.consistency != ReadConsistency.boundedStaleness ||
                    (r['lag_ms'] as num? ?? double.infinity) <=
                        options!.maxLag.inMilliseconds)
                .toList();
        final address = pinned ??
            (safeRead && eligible.isNotEmpty
                ? eligible[next++ % eligible.length]['addr'] as String
                : primary);
        if (policy.$1 == 'begin' || policy.$1 == 'session') {
          pinned = address;
          session |= policy.$1 == 'session';
        }
        try {
          final result = await operation(await _endpoint(address));
          if (policy.$1 == 'end' && !session) pinned = null;
          return result;
        } catch (error) {
          if (!_network(error)) rethrow;
          refreshAt = 0;
          await clients.remove(address)?.close();
          if (pinned != null) broken = true;
          if (pinned != null || !safeRead) rethrow;
          await refresh();
          return operation(await _endpoint(primary));
        }
      });
  @override
  Future<Uint8List> query(String sql, {List<Object?>? params}) =>
      _invoke((c) => c.query(sql, params: params), sql: sql);
  Future<Uint8List> queryWithOptions(String sql, ReadOptions options,
          {List<Object?>? params}) =>
      _invoke((c) => c.query(sql, params: params), sql: sql, options: options);
  @override
  Future<Uint8List> insert(String collection, Map<String, dynamic> payload) =>
      _invoke((c) => c.insert(collection, payload));
  @override
  Future<Uint8List> bulkInsert(
          String collection, List<Map<String, dynamic>> rows) =>
      _invoke((c) => c.bulkInsert(collection, rows));
  @override
  Future<Uint8List> get(String collection, String id) =>
      _invoke((c) => c.get(collection, id), read: true);
  @override
  Future<Uint8List> delete(String collection, String id) =>
      _invoke((c) => c.delete(collection, id));
  @override
  Future<void> ping() => _invoke((c) => c.ping());
  @override
  Future<void> close() => _locked(() async {
        closed = true;
        for (final client in clients.values) {
          await client.close();
        }
        clients.clear();
      });
}
