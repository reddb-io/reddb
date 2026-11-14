import 'dart:convert';
import 'dart:async';
import 'dart:math';
import 'dart:typed_data';

import 'errors.dart';

// SDK Helper Spec v1.0 — rich helper surface on top of the transport-agnostic
// Conn. Helpers compile SQL strings against the engine; the same wire request
// works across RedWire and HTTP. See docs/spec/sdk-helpers.md.

/// Minimal contract helpers need. `Conn` and `Reddb` both satisfy it via
/// `query`; tests pass fakes that record SQL.
abstract class Querier {
  Future<Uint8List> query(String sql, {List<Object?>? params});
}

/// Groups the rich namespaces (`documents`, `kv`, `queues`, `tx`) bound to a
/// single transport. Helpers are stateless — safe to construct per call.
class Helpers {
  Helpers(this._q);

  /// SDK Helper Spec version this driver implements.
  /// See `docs/spec/sdk-helpers.md` §14.
  static const String helperSpecVersion = '1.0';

  final Querier _q;

  DocumentClient get documents => DocumentClient(_q);

  /// KV namespace bound to the default collection (``kv_default``).
  KvClient get kv => KvClient(_q);

  /// KV client bound to one explicit collection.
  KvClient kvNamed(String collection) => KvClient(_q, collection: collection);

  SecretsClient secrets(String collection) => SecretsClient(_q, collection);

  /// Queue namespace (spec-canonical alias of [queue]).
  QueueClient get queues => QueueClient(_q);

  /// Single-handle queue namespace (legacy singular alias).
  QueueClient get queue => QueueClient(_q);

  /// Queue client bound to one explicit queue name.
  ScopedQueueClient queueNamed(String name) => QueueClient(_q).scope(name);

  /// Imperative transaction client (`begin`/`commit`/`rollback` +
  /// optional callback form). See spec §7.
  TxClient tx() => TxClient(_q);
}

// --- Envelopes -------------------------------------------------------

class InsertResult {
  InsertResult({required this.affected, required this.rid, this.item});

  final int affected;
  final String rid;
  final Map<String, Object?>? item;
}

class DeleteResult {
  DeleteResult(this.affected) : deleted = affected > 0;

  final int affected;

  /// Per spec §2.4: `deleted == affected > 0`. A delete of a missing item
  /// surfaces `{affected: 0, deleted: false}` and is NOT an error.
  final bool deleted;
}

class BulkInsertResult {
  BulkInsertResult({required this.affected, required this.rids});

  final int affected;
  final List<String> rids;
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

class SecretsClient {
  SecretsClient(this._q, this.collection) {
    _vaultPart(collection, 'vault collection');
  }

  final Querier _q;
  final String collection;

  Future<void> put(String key, Object? value) async {
    await _q.query('VAULT PUT ${_path(key)} = ${_kvValueLiteral(value)}');
  }

  Future<Map<String, Object?>?> get(String key) => _read('GET', key);
  Future<Map<String, Object?>?> reveal(String key) => _read('REVEAL', key);

  Future<Map<String, Object?>?> _read(String verb, String key) async {
    final body = await _q.query('VAULT $verb ${_path(key)}');
    return _firstRow(body).$1;
  }

  String _path(String key) {
    _vaultPart(key, 'vault key');
    return '$collection.$key';
  }

  static void _vaultPart(String value, String label) {
    if (!RegExp(r'^[A-Za-z0-9_.]+$').hasMatch(value)) {
      throw InvalidArgument('invalid $label');
    }
  }
}

class QueueMessage {
  QueueMessage({
    required this.messageId,
    required this.deliveryId,
    required this.payload,
    required this.consumer,
    required this.deliveryCount,
    required this.raw,
  });

  final String messageId;
  final String deliveryId;
  final Object? payload;
  final String? consumer;
  final int deliveryCount;
  final Map<String, Object?> raw;
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
      'INSERT INTO ${_sqlIdentifierPath(collection)} DOCUMENT '
      'VALUES (${_jsonInlineLiteral(document)}) RETURNING *',
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
    if (patch.isEmpty) {
      throw InvalidArgument(
          'documents.patch requires a non-empty patch object');
    }
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
      'UPDATE ${_sqlIdentifierPath(collection)} SET '
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

  /// Bulk insert. Empty payloads is a no-op. Identity (`rids`) preserved in
  /// input order per spec §3.4.
  Future<BulkInsertResult> bulkInsert(
    String collection,
    List<Map<String, Object?>> payloads,
  ) async {
    if (payloads.isEmpty) {
      return BulkInsertResult(affected: 0, rids: const []);
    }
    await _ensureCollection(collection);
    final rids = <String>[];
    for (final p in payloads) {
      final body = await _q.query(
        'INSERT INTO ${_sqlIdentifierPath(collection)} DOCUMENT '
        'VALUES (${_jsonInlineLiteral(p)}) RETURNING *',
      );
      final (row, _) = _firstRow(body);
      if (row == null || row['rid'] == null) {
        throw InvalidResponse(
          'documents.bulk_insert expected returned rid per row',
        );
      }
      rids.add(_ridString(row['rid'])!);
    }
    return BulkInsertResult(affected: rids.length, rids: rids);
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
      put(key, value, collection: collection, tags: tags, expireMs: expireMs);

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
    final expire =
        (expireMs != null && expireMs > 0) ? ' EXPIRE $expireMs ms' : '';
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

  ScopedQueueClient scope(String name) {
    _assertIdentifier(name, 'queue name');
    return ScopedQueueClient(this, name);
  }

  /// Idempotent `CREATE QUEUE IF NOT EXISTS`.
  Future<void> create(String queue) async {
    _assertIdentifier(queue, 'queue name');
    await _q.query('CREATE QUEUE IF NOT EXISTS ${_sqlIdentifier(queue)}');
  }

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

  /// Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks
  /// until a message is available for [consumer] on [queue], the
  /// [wait] budget elapses, or the server cancels. Timeout returns
  /// an empty list — same shape as an empty [pop]; never throws.
  /// [wait] is required; there is no infinite-wait default.
  /// Cancellation and cap rejection surface via the transport.
  Future<List<Object?>> readWait(
    String queue,
    String consumer, {
    required Duration wait,
    String? group,
    int? count,
  }) async {
    final messages = await readWaitEntries(
      queue,
      consumer,
      wait: wait,
      group: group,
      count: count,
    );
    return messages.map((message) => message.payload).toList();
  }

  Future<List<QueueMessage>> readWaitEntries(
    String queue,
    String consumer, {
    required Duration wait,
    String? group,
    int? count,
  }) async {
    _assertIdentifier(queue, 'queue name');
    _assertIdentifier(consumer, 'consumer name');
    if (wait.isNegative) {
      throw InvalidArgument(
        'queue readWait requires a non-negative wait duration (no infinite wait)',
      );
    }
    var groupClause = '';
    if (group != null && group.isNotEmpty) {
      _assertIdentifier(group, 'group name');
      groupClause = ' GROUP ${_sqlIdentifier(group)}';
    }
    var countClause = '';
    if (count != null) {
      if (count < 0) {
        throw InvalidArgument('queue count must be a non-negative integer');
      }
      countClause = ' COUNT $count';
    }
    final waitMs = wait.inMilliseconds;
    final sql =
        'QUEUE READ ${_sqlIdentifier(queue)}$groupClause CONSUMER ${_sqlIdentifier(consumer)}$countClause WAIT ${waitMs}ms';
    final body = await _q.query(sql);
    return _allRows(body)
        .map(
          (row) => QueueMessage(
            messageId: row['message_id']?.toString() ?? '',
            deliveryId: row['delivery_id']?.toString() ?? '',
            payload: row['payload'],
            consumer: row['consumer']?.toString(),
            deliveryCount: (row['delivery_count'] as num?)?.toInt() ?? 0,
            raw: row,
          ),
        )
        .toList();
  }

  Future<void> ack(String queue, String deliveryId) async {
    if (deliveryId.isEmpty) {
      throw InvalidArgument('queue delivery_id must be non-empty');
    }
    await _q.query(
      'QUEUE ACK ${_sqlIdentifier(queue)} WITH delivery_id = '
      '${_kvTagLiteral(deliveryId)}',
    );
  }

  Future<void> nack(
    String queue,
    String deliveryId, {
    String? delay,
  }) async {
    if (deliveryId.isEmpty) {
      throw InvalidArgument('queue delivery_id must be non-empty');
    }
    var delayClause = '';
    if (delay != null && delay.isNotEmpty) {
      if (!RegExp(
        r'^\d+(?:\.\d+)?(?:ms|s|sec|secs|m|min|mins|h|hr|hrs|d|day|days)?$',
      ).hasMatch(delay)) {
        throw InvalidArgument('invalid queue retry delay');
      }
      delayClause = ' WITH DELAY $delay';
    }
    await _q.query(
      'QUEUE NACK ${_sqlIdentifier(queue)} WITH delivery_id = '
      '${_kvTagLiteral(deliveryId)}$delayClause',
    );
  }

  Future<void> extend(String queue, String deliveryId) async {
    if (deliveryId.isEmpty) {
      throw InvalidArgument('queue delivery_id must be non-empty');
    }
    await _q.query(
      'QUEUE EXTEND ${_sqlIdentifier(queue)} WITH delivery_id = '
      '${_kvTagLiteral(deliveryId)}',
    );
  }
}

class ScopedQueueClient {
  ScopedQueueClient(this._queue, this.name);

  final QueueClient _queue;
  final String name;

  Future<QueuePushResult> push(Object? value, {int? priority}) =>
      _queue.push(name, value, priority: priority);
  Future<List<Object?>> pop({int? count}) => _queue.pop(name, count: count);
  Future<List<Object?>> peek({int? count}) => _queue.peek(name, count: count);
  Future<int> len() => _queue.len(name);
  Future<DeleteResult> purge() => _queue.purge(name);
  Future<List<QueueMessage>> readWait(
    String consumer, {
    required Duration wait,
    String? group,
    int? count,
  }) =>
      _queue.readWaitEntries(
        name,
        consumer,
        wait: wait,
        group: group,
        count: count,
      );
  Future<void> ack(String deliveryId) => _queue.ack(name, deliveryId);
  Future<void> nack(String deliveryId, {String? delay}) =>
      _queue.nack(name, deliveryId, delay: delay);
  Future<void> extend(String deliveryId) => _queue.extend(name, deliveryId);
  QueueConsumer consumer(QueueConsumerOptions options) =>
      QueueConsumer(this, options);
}

typedef QueueMessageHandler = Future<void> Function(QueueMessage message);
typedef QueueContextMessageHandler = Future<void> Function(
  QueueMessage message,
  QueueHandlerContext context,
);
typedef QueueErrorHandler = Future<void> Function(
  Object error,
  QueueMessage? message,
);

class QueueReceiveBackoff {
  const QueueReceiveBackoff({
    this.initial = const Duration(milliseconds: 250),
    this.max = const Duration(seconds: 30),
    this.multiplier = 2,
    this.jitter = true,
  });

  final Duration initial;
  final Duration max;
  final double multiplier;
  final bool jitter;
}

class QueueHandlerContext {
  QueueHandlerContext(this._extend);

  final Future<void> Function() _extend;
  bool _cancelled = false;

  bool get isCancelled => _cancelled;
  Future<void> extendLease() => _extend();
  void _cancel() => _cancelled = true;
}

class QueueConsumerOptions {
  QueueConsumerOptions({
    required this.consumer,
    this.handler,
    this.contextHandler,
    this.group,
    this.wait = const Duration(seconds: 30),
    this.retryDelay,
    this.onError,
    this.concurrency = 1,
    this.receiveBackoff = const QueueReceiveBackoff(),
    this.leaseHeartbeat,
    this.handlerTimeout,
  });

  final String consumer;
  final String? group;
  final Duration wait;
  final String? retryDelay;
  final QueueMessageHandler? handler;
  final QueueContextMessageHandler? contextHandler;
  final QueueErrorHandler? onError;
  final int concurrency;
  final QueueReceiveBackoff receiveBackoff;
  final Duration? leaseHeartbeat;
  final Duration? handlerTimeout;
}

enum QueueConsumerState { idle, running, paused, stopping, stopped, failed }

class QueueConsumerStatus {
  const QueueConsumerStatus({
    required this.state,
    required this.isRunning,
    required this.isPaused,
    required this.inFlight,
    required this.concurrency,
  });

  final QueueConsumerState state;
  final bool isRunning;
  final bool isPaused;
  final int inFlight;
  final int concurrency;
}

/// Managed at-least-once worker with bounded concurrency and renewable leases.
class QueueConsumer {
  QueueConsumer(this._queue, this._options) {
    _assertIdentifier(_options.consumer, 'consumer name');
    final group = _options.group;
    if (group != null && group.isNotEmpty) {
      _assertIdentifier(group, 'group name');
    }
    if (_options.wait <= Duration.zero) {
      throw InvalidArgument('queue consumer wait must be greater than zero');
    }
    if (_options.handler == null && _options.contextHandler == null) {
      throw InvalidArgument('queue consumer requires a handler');
    }
    if (_options.handler != null && _options.contextHandler != null) {
      throw InvalidArgument('queue consumer accepts only one handler');
    }
    if (_options.concurrency < 1 || _options.concurrency > 128) {
      throw InvalidArgument(
          'queue consumer concurrency must be between 1 and 128');
    }
    final backoff = _options.receiveBackoff;
    if (backoff.initial <= Duration.zero ||
        backoff.max < backoff.initial ||
        !backoff.multiplier.isFinite ||
        backoff.multiplier < 1) {
      throw InvalidArgument('invalid queue receive backoff');
    }
    if (_options.leaseHeartbeat != null &&
        _options.leaseHeartbeat! <= Duration.zero) {
      throw InvalidArgument('queue lease heartbeat must be greater than zero');
    }
    if (_options.handlerTimeout != null &&
        _options.handlerTimeout! <= Duration.zero) {
      throw InvalidArgument('queue handler timeout must be greater than zero');
    }
  }

  final ScopedQueueClient _queue;
  final QueueConsumerOptions _options;
  bool _stopping = false;
  bool _aborted = false;
  bool _paused = false;
  int _inFlight = 0;
  QueueConsumerState _state = QueueConsumerState.idle;
  Future<void>? _completion;
  Completer<void> _changed = Completer<void>();
  final Random _random = Random();

  QueueConsumerStatus get status => QueueConsumerStatus(
        state: _state,
        isRunning: _state == QueueConsumerState.running ||
            _state == QueueConsumerState.paused,
        isPaused: _paused,
        inFlight: _inFlight,
        concurrency: _options.concurrency,
      );

  QueueConsumer start() {
    if (_completion != null) {
      throw InvalidArgument('queue consumer already started');
    }
    _state = QueueConsumerState.running;
    _completion = _run().then(
      (_) => _state = QueueConsumerState.stopped,
      onError: (Object error, StackTrace stack) {
        _state = QueueConsumerState.failed;
        Error.throwWithStackTrace(error, stack);
      },
    );
    return this;
  }

  Future<void> stop() async {
    requestStop();
    await _completion;
  }

  void requestStop() {
    _stopping = true;
    _paused = false;
    if (_state != QueueConsumerState.idle &&
        _state != QueueConsumerState.stopped &&
        _state != QueueConsumerState.failed) {
      _state = QueueConsumerState.stopping;
    }
    _signal();
  }

  void abort() {
    _aborted = true;
    requestStop();
  }

  void pause() {
    if (_state == QueueConsumerState.running) {
      _paused = true;
      _state = QueueConsumerState.paused;
    }
  }

  void resume() {
    if (_state == QueueConsumerState.paused) {
      _paused = false;
      _state = QueueConsumerState.running;
      _signal();
    }
  }

  Future<void> get completion => _completion ?? Future.value();

  Future<void> _run() async {
    await Future.wait(List.generate(_options.concurrency, (_) => _runLane()));
  }

  Future<void> _runLane() async {
    var receiveFailures = 0;
    while (!_stopping) {
      await _waitUntilReady();
      if (_stopping) return;
      List<QueueMessage> messages;
      try {
        messages = await _queue.readWait(
          _options.consumer,
          wait: _options.wait,
          group: _options.group,
          count: 1,
        );
      } catch (error) {
        final onError = _options.onError;
        if (onError == null) rethrow;
        await onError(error, null);
        receiveFailures++;
        if (!await _backoff(receiveFailures)) return;
        continue;
      }
      receiveFailures = 0;
      if (messages.isEmpty) continue;
      await _process(messages.first);
    }
  }

  Future<void> _process(QueueMessage message) async {
    if (message.deliveryId.isEmpty) {
      await _report(
        InvalidResponse('queue delivery is missing delivery_id'),
        message,
      );
      return;
    }
    _inFlight++;
    final context =
        QueueHandlerContext(() => _queue.extend(message.deliveryId));
    Object? handlerError;
    Object? leaseError;
    var timedOut = false;
    var settled = false;
    final timeout = _options.handlerTimeout == null
        ? null
        : Timer(_options.handlerTimeout!, () {
            timedOut = true;
            context._cancel();
          });
    final heartbeat = _heartbeat(message, context, () => settled, (error) {
      leaseError = error;
      context._cancel();
    });
    try {
      final contextHandler = _options.contextHandler;
      if (contextHandler != null) {
        await contextHandler(message, context);
      } else {
        await _options.handler!(message);
      }
    } catch (error) {
      handlerError = error;
    } finally {
      settled = true;
      timeout?.cancel();
      await heartbeat;
      _inFlight--;
    }
    if (_aborted) return;
    if (leaseError != null) {
      await _report(leaseError!, message);
      return;
    }
    if (timedOut) {
      final error = QueueHandlerTimeout(
        'queue handler exceeded ${_options.handlerTimeout}',
      );
      try {
        await _queue.nack(message.deliveryId, delay: _options.retryDelay);
      } catch (nackError) {
        await _report(nackError, message);
        return;
      }
      await _report(error, message);
      return;
    }
    if (handlerError != null) {
      try {
        await _queue.nack(message.deliveryId, delay: _options.retryDelay);
      } catch (nackError) {
        await _report(nackError, message);
        return;
      }
      await _report(handlerError, message);
      return;
    }
    try {
      await _queue.ack(message.deliveryId);
    } catch (error) {
      // An ambiguous ACK must never be followed by a NACK.
      await _report(error, message);
    }
  }

  Future<void> _heartbeat(
    QueueMessage message,
    QueueHandlerContext context,
    bool Function() settled,
    void Function(Object error) failed,
  ) async {
    final period = _options.leaseHeartbeat;
    if (period == null) return;
    while (!settled()) {
      await Future<void>.delayed(period);
      if (settled()) return;
      try {
        await context.extendLease();
      } catch (error) {
        failed(error);
        return;
      }
    }
  }

  Future<void> _report(Object error, QueueMessage? message) async {
    final onError = _options.onError;
    if (onError == null) throw error;
    await onError(error, message);
  }

  Future<void> _waitUntilReady() async {
    while (_paused && !_stopping) {
      final changed = _changed.future;
      if (!_paused || _stopping) return;
      await changed;
    }
  }

  Future<bool> _backoff(int failures) async {
    final options = _options.receiveBackoff;
    final factor = pow(options.multiplier, failures - 1).toDouble();
    var milliseconds = min(
      options.initial.inMilliseconds * factor,
      options.max.inMilliseconds.toDouble(),
    );
    if (options.jitter) milliseconds *= _random.nextDouble();
    final timer = Future<void>.delayed(
      Duration(milliseconds: milliseconds.round()),
    );
    await Future.any([timer, _changed.future]);
    return !_stopping;
  }

  void _signal() {
    if (!_changed.isCompleted) _changed.complete();
    _changed = Completer<void>();
  }
}

// --- Transactions ----------------------------------------------------

/// Callback signature for [TxClient.run].
typedef TxBody = Future<void> Function(TxClient tx);

/// Imperative + optional callback transactions per spec §7.
///
/// Nested [run] calls are rejected with `INVALID_ARGUMENT`; callers who need
/// savepoints should issue them directly via `conn.query`. Matches the Java
/// and Rust driver decision so cross-driver docs stay consistent.
class TxClient {
  TxClient(this._q);

  final Querier _q;
  bool _open = false;
  bool _inRun = false;

  Future<void> begin() async {
    await _q.query('BEGIN');
    _open = true;
  }

  Future<void> commit() async {
    await _q.query('COMMIT');
    _open = false;
  }

  Future<void> rollback() async {
    await _q.query('ROLLBACK');
    _open = false;
  }

  /// Run [body] inside a transaction. Successful body commits; a thrown
  /// error rolls back and re-throws.
  Future<void> run(TxBody body) async {
    if (_inRun) {
      throw InvalidArgument('nested tx.run is not supported');
    }
    _inRun = true;
    try {
      await begin();
      try {
        await body(this);
        await commit();
      } catch (_) {
        if (_open) {
          try {
            await rollback();
          } catch (_) {
            // swallow secondary rollback failure to preserve original error
          }
        }
        rethrow;
      }
    } finally {
      _inRun = false;
    }
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
  return "'${value.replaceAll('\\', '\\\\').replaceAll("'", "''")}'";
}

String _kvValueLiteral(Object? value) {
  if (value == null) return 'NULL';
  if (value is bool) return value ? 'true' : 'false';
  if (value is num) return value.toString();
  if (value is String) {
    return "'${value.replaceAll('\\', '\\\\').replaceAll("'", "''")}'";
  }
  final s = jsonEncode(value);
  return "'${s.replaceAll('\\', '\\\\').replaceAll("'", "''")}'";
}

String _kvTagLiteral(String tag) =>
    "'${tag.replaceAll('\\', '\\\\').replaceAll("'", "''")}'";

String _queueValueLiteral(Object? value) {
  if (value == null) return 'NULL';
  if (value is bool) return value ? 'true' : 'false';
  if (value is num) return value.toString();
  if (value is String) {
    return "'${value.replaceAll('\\', '\\\\').replaceAll("'", "''")}'";
  }
  return jsonEncode(value);
}

String _valueLiteral(Object? value) => _kvValueLiteral(value);

/// ADR 0067 (#1709): a document body is written as an inline strict-JSON
/// literal (no surrounding quotes) — the quoted-string coercion is removed.
String _jsonInlineLiteral(Object? value) => jsonEncode(value);

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
