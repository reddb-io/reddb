import 'dart:convert';
import 'dart:typed_data';

import 'errors.dart';

// SDK Helper Spec v0.1 — rich helper surface on top of the transport-agnostic
// Conn. Helpers compile SQL strings against the engine; the same wire request
// works across RedWire and HTTP. See docs/clients/sdk-helper-spec.md.

/// Minimal contract helpers need. `Conn` and `Reddb` both satisfy it via
/// `query`; tests pass fakes that record SQL.
abstract class Querier {
  Future<Uint8List> query(String sql, {List<Object?>? params});
}

/// Groups the rich namespaces (`documents`, `kv`, `queue`) bound to a single
/// transport. Helpers are stateless — safe to construct per call.
class Helpers {
  Helpers(this._q);

  final Querier _q;

  DocumentClient get documents => DocumentClient(_q);

  /// KV namespace bound to the default collection (``kv_default``).
  KvClient get kv => KvClient(_q);

  QueueClient get queue => QueueClient(_q);
}

// --- Envelopes -------------------------------------------------------

class InsertResult {
  InsertResult({required this.affected, required this.rid, this.item});

  final int affected;
  final String rid;
  final Map<String, Object?>? item;
}

class DeleteResult {
  DeleteResult(this.affected);

  final int affected;
}

class ExistsResult {
  ExistsResult(this.exists);

  final bool exists;
}

class ListResult {
  ListResult({required this.items, this.nextCursor});

  final List<Map<String, Object?>> items;
  final String? nextCursor;
}

class QueuePushResult {
  QueuePushResult({required this.affected, this.rid});

  final int affected;
  final String? rid;
}

// --- Documents -------------------------------------------------------

class DocumentClient {
  DocumentClient(this._q);

  final Querier _q;

  Future<InsertResult> insert(
    String collection,
    Map<String, Object?> document,
  ) async {
    await _ensureCollection(collection);
    final body = await _q.query(
      'INSERT INTO ${_sqlIdentifierPath(collection)} DOCUMENT (body) '
      'VALUES (${_jsonLiteral(document)}) RETURNING *',
    );
    final (row, declared) = _firstRow(body);
    if (row == null || row['rid'] == null) {
      throw InvalidResponse(
        'documents.insert expected one returned item with rid',
      );
    }
    final affected = declared == 0 ? 1 : declared;
    return InsertResult(
      affected: affected,
      rid: _ridString(row['rid'])!,
      item: row,
    );
  }

  Future<Map<String, Object?>> get(String collection, String rid) async {
    final body = await _q.query(
      'SELECT * FROM ${_sqlIdentifierPath(collection)} WHERE rid = \$1 LIMIT 1',
      params: [rid],
    );
    final (row, _) = _firstRow(body);
    if (row == null) {
      throw NotFound('document "$rid" was not found');
    }
    return row;
  }

  Future<ListResult> list(
    String collection, {
    int? limit,
    String? orderBy,
    String? filter,
  }) async {
    final lim = _normalizeLimit(limit);
    final order = (orderBy == null || orderBy.isEmpty) ? 'rid ASC' : orderBy;
    final where = (filter == null || filter.isEmpty) ? '' : ' WHERE $filter';
    final body = await _q.query(
      'SELECT * FROM ${_sqlIdentifierPath(collection)}$where '
      'ORDER BY $order LIMIT $lim',
    );
    return ListResult(items: _allRows(body));
  }

  Future<Map<String, Object?>> patch(
    String collection,
    String rid,
    Map<String, Object?> patch,
  ) async {
    if (patch.isEmpty) return get(collection, rid);
    final parts = <String>[];
    patch.forEach((field, value) {
      if (field.contains('/')) {
        throw InvalidArgument(
          'documents.patch currently accepts top-level document fields',
        );
      }
      parts.add('${_sqlIdentifier(field)} = ${_valueLiteral(value)}');
    });
    final body = await _q.query(
      'UPDATE ${_sqlIdentifierPath(collection)} DOCUMENTS SET '
      '${parts.join(', ')} WHERE rid = \$1 RETURNING *',
      params: [rid],
    );
    final (row, _) = _firstRow(body);
    if (row == null) {
      throw NotFound('document "$rid" was not found');
    }
    return row;
  }

  Future<DeleteResult> delete(String collection, String rid) async {
    final body = await _q.query(
      'DELETE FROM ${_sqlIdentifierPath(collection)} WHERE rid = \$1',
      params: [rid],
    );
    return DeleteResult(_affectedFromBody(body));
  }

  Future<void> _ensureCollection(String collection) async {
    try {
      await _q.query('CREATE DOCUMENT ${_sqlIdentifierPath(collection)}');
    } catch (e) {
      if (e.toString().contains('already exists')) return;
      rethrow;
    }
  }
}

// --- KV --------------------------------------------------------------

class KvClient {
  KvClient(this._q, {this.collection = 'kv_default'});

  final Querier _q;
  final String collection;

  Future<void> set(
    String key,
    Object? value, {
    String? collection,
    List<String>? tags,
    int? expireMs,
  }) =>
      put(key, value,
          collection: collection, tags: tags, expireMs: expireMs);

  Future<void> put(
    String key,
    Object? value, {
    String? collection,
    List<String>? tags,
    int? expireMs,
  }) async {
    final coll = (collection == null || collection.isEmpty)
        ? this.collection
        : collection;
    final lit = _kvValueLiteral(value);
    final expire = (expireMs != null && expireMs > 0)
        ? ' EXPIRE $expireMs ms'
        : '';
    final tagClause = (tags == null || tags.isEmpty)
        ? ''
        : ' TAGS [${tags.map(_kvTagLiteral).join(', ')}]';
    final path = kvPath(coll, key);
    await _q.query('KV PUT $path = $lit$expire$tagClause');
  }

  Future<Object?> get(String key, {String? collection}) async {
    final coll = (collection == null || collection.isEmpty)
        ? this.collection
        : collection;
    final path = kvPath(coll, key);
    final body = await _q.query('KV GET $path');
    final (row, _) = _firstRow(body);
    if (row == null) return null;
    return row['value'];
  }

  Future<ExistsResult> exists(String key, {String? collection}) async {
    final v = await get(key, collection: collection);
    return ExistsResult(v != null);
  }

  Future<DeleteResult> delete(String key, {String? collection}) async {
    final coll = (collection == null || collection.isEmpty)
        ? this.collection
        : collection;
    final path = kvPath(coll, key);
    final body = await _q.query('KV DELETE $path');
    return DeleteResult(_affectedFromBody(body));
  }

  Future<ListResult> list({
    String? collection,
    int? limit,
    String? prefix,
  }) async {
    final coll = (collection == null || collection.isEmpty)
        ? this.collection
        : collection;
    final lim = _normalizeLimit(limit);
    final body = await _q.query(
      'SELECT key, value FROM ${_sqlIdentifier(coll)} '
      'ORDER BY key ASC LIMIT $lim',
    );
    var rows = _allRows(body);
    if (prefix != null && prefix.isNotEmpty) {
      rows = rows.where((r) {
        final k = r['key'];
        return k is String && k.startsWith(prefix);
      }).toList();
    }
    return ListResult(items: rows);
  }
}

// --- Queue -----------------------------------------------------------

class QueueClient {
  QueueClient(this._q);

  final Querier _q;

  Future<QueuePushResult> push(
    String queue,
    Object? value, {
    int? priority,
  }) async {
    _assertIdentifier(queue, 'queue name');
    final lit = _queueValueLiteral(value);
    final prio = priority == null ? '' : ' PRIORITY $priority';
    final body = await _q.query(
      'QUEUE PUSH ${_sqlIdentifier(queue)} $lit$prio',
    );
    final affected = _affectedFromBody(body);
    final (row, _) = _firstRow(body);
    return QueuePushResult(
      affected: affected == 0 ? 1 : affected,
      rid: row == null ? null : _ridString(row['rid']),
    );
  }

  Future<List<Object?>> pop(String queue, {int? count}) =>
      _fetch('POP', queue, count);

  Future<List<Object?>> peek(String queue, {int? count}) =>
      _fetch('PEEK', queue, count);

  Future<List<Object?>> _fetch(String verb, String queue, int? count) async {
    _assertIdentifier(queue, 'queue name');
    var suffix = '';
    if (count != null) {
      if (count < 0) {
        throw InvalidArgument('queue count must be a non-negative integer');
      }
      suffix = ' COUNT $count';
    }
    final body = await _q.query(
      'QUEUE $verb ${_sqlIdentifier(queue)}$suffix',
    );
    return _allRows(body).map((r) => r['payload']).toList();
  }

  Future<int> len(String queue) async {
    _assertIdentifier(queue, 'queue name');
    final body = await _q.query('QUEUE LEN ${_sqlIdentifier(queue)}');
    final (row, _) = _firstRow(body);
    if (row == null) return 0;
    final v = row['len'];
    if (v is int) return v;
    if (v is num) return v.toInt();
    return 0;
  }

  Future<DeleteResult> purge(String queue) async {
    _assertIdentifier(queue, 'queue name');
    final body = await _q.query('QUEUE PURGE ${_sqlIdentifier(queue)}');
    return DeleteResult(_affectedFromBody(body));
  }
}

// --- pure SQL helpers (unit-testable) --------------------------------

/// Builds a fully qualified ``collection.key`` reference, quoting the key
/// segment when it contains anything but `[A-Za-z0-9_]`.
String kvPath(String collection, String key) {
  for (final ch in collection.codeUnits) {
    if (!_isIdentChar(ch)) {
      throw InvalidArgument(
        'invalid KV collection "$collection": character '
        '"${String.fromCharCode(ch)}" is not supported',
      );
    }
  }
  return '$collection.${_kvKeySegment(key)}';
}

String _kvKeySegment(String value) {
  if (value.isNotEmpty && _allIdentChars(value)) return value;
  return "'${value.replaceAll("'", "''")}'";
}

String _kvValueLiteral(Object? value) {
  if (value == null) return 'NULL';
  if (value is bool) return value ? 'true' : 'false';
  if (value is num) return value.toString();
  if (value is String) {
    return "'${value.replaceAll("'", "''")}'";
  }
  final s = jsonEncode(value);
  return "'${s.replaceAll("'", "''")}'";
}

String _kvTagLiteral(String tag) => "'${tag.replaceAll("'", "''")}'";

String _queueValueLiteral(Object? value) {
  if (value == null) return 'NULL';
  if (value is bool) return value ? 'true' : 'false';
  if (value is num) return value.toString();
  if (value is String) {
    return "'${value.replaceAll("'", "''")}'";
  }
  return jsonEncode(value);
}

String _valueLiteral(Object? value) => _kvValueLiteral(value);

String _jsonLiteral(Object? value) {
  final s = jsonEncode(value);
  return "'${s.replaceAll("'", "''")}'";
}

String _sqlIdentifier(String value) {
  if (value.isNotEmpty && _allIdentChars(value)) return value;
  return '"${value.replaceAll('"', '""')}"';
}

String _sqlIdentifierPath(String value) {
  if (!value.contains('.')) return _sqlIdentifier(value);
  return value.split('.').map(_sqlIdentifier).join('.');
}

void _assertIdentifier(String value, String label) {
  if (value.isEmpty || !_allIdentChars(value)) {
    throw InvalidArgument(
      'invalid $label "$value": must match [A-Za-z0-9_]+',
    );
  }
}

int _normalizeLimit(int? value) {
  if (value == null || value == 0) return 100;
  if (value < 0) {
    throw InvalidArgument('limit must be a positive integer');
  }
  return value;
}

bool _isIdentChar(int r) =>
    (r >= 0x61 && r <= 0x7A) || // a-z
    (r >= 0x41 && r <= 0x5A) || // A-Z
    (r >= 0x30 && r <= 0x39) || // 0-9
    r == 0x5F; // _

bool _allIdentChars(String s) {
  for (final ch in s.codeUnits) {
    if (!_isIdentChar(ch)) return false;
  }
  return true;
}

// --- response parsing -------------------------------------------------

Map<String, Object?>? _decodeBody(Uint8List body) {
  if (body.isEmpty) return null;
  try {
    final obj = jsonDecode(utf8.decode(body));
    if (obj is Map<String, Object?>) return obj;
    return null;
  } catch (_) {
    return null;
  }
}

int _affectedFromMap(Map<String, Object?> obj) {
  final v = obj['affected'];
  if (v is int) return v;
  if (v is num) return v.toInt();
  return 0;
}

(Map<String, Object?>?, int) _firstRow(Uint8List body) {
  final obj = _decodeBody(body);
  if (obj == null) return (null, 0);
  var affected = _affectedFromMap(obj);
  var rows = obj['rows'];
  if (rows is! List || rows.isEmpty) {
    final nested = obj['result'];
    if (nested is Map<String, Object?>) {
      rows = nested['rows'];
      if (affected == 0) affected = _affectedFromMap(nested);
    }
  }
  if (rows is! List || rows.isEmpty) return (null, affected);
  final first = rows.first;
  if (first is Map<String, Object?>) return (first, affected);
  return (null, affected);
}

List<Map<String, Object?>> _allRows(Uint8List body) {
  final obj = _decodeBody(body);
  if (obj == null) return const [];
  Object? raw = obj['rows'];
  if (raw is! List) {
    final nested = obj['result'];
    if (nested is Map<String, Object?>) raw = nested['rows'];
  }
  if (raw is! List) return const [];
  return [
    for (final r in raw)
      if (r is Map<String, Object?>) r,
  ];
}

int _affectedFromBody(Uint8List body) {
  final obj = _decodeBody(body);
  if (obj == null) return 0;
  final direct = _affectedFromMap(obj);
  if (direct > 0) return direct;
  final nested = obj['result'];
  if (nested is Map<String, Object?>) return _affectedFromMap(nested);
  return 0;
}

String? _ridString(Object? value) {
  if (value is String) return value;
  if (value is num) {
    if (value is int) return value.toString();
    return value.toString();
  }
  return null;
}
