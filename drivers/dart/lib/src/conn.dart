import 'dart:typed_data';

/// Common surface for both transports. The `Reddb` facade holds one
/// of these and routes calls.
abstract class Conn {
  /// Run a SQL string. Returns the raw JSON envelope as bytes — caller
  /// decodes with `dart:convert` (`jsonDecode(utf8.decode(bytes))`).
  Future<Uint8List> query(String sql);

  /// Insert a single row. `payload` is encoded JSON (object).
  Future<Uint8List> insert(String collection, Map<String, dynamic> payload);

  /// Bulk-insert rows. Each entry is an object.
  Future<Uint8List> bulkInsert(
    String collection,
    List<Map<String, dynamic>> rows,
  );

  /// Get one row by primary id.
  Future<Uint8List> get(String collection, String id);

  /// Delete one row by primary id.
  Future<Uint8List> delete(String collection, String id);

  /// Lightweight liveness check.
  Future<void> ping();

  /// Best-effort clean shutdown. Idempotent.
  Future<void> close();
}
