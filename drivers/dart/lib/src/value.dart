import 'dart:typed_data';

/// Explicit wrappers for parameter values whose Dart native shape is ambiguous.
///
/// Plain strings bind as Text and `List<double>` binds as Vector, so JSON arrays
/// and UUIDs use wrappers to select the corresponding engine Value tag.
class Value {
  const Value._(this.kind, this.value);

  static const String kindBytes = 'bytes';
  static const String kindJson = 'json';
  static const String kindUuid = 'uuid';

  final String kind;
  final Object? value;

  static Value bytes(List<int> bytes) =>
      Value._(kindBytes, Uint8List.fromList(bytes));

  static Value json(Object? value) => Value._(kindJson, value);

  static Value uuid(String uuid) => Value._(kindUuid, uuid);
}
