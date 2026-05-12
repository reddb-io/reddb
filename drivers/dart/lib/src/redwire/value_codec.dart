import 'dart:collection';
import 'dart:convert';
import 'dart:typed_data';

import '../value.dart';
import 'frame.dart';

/// Parameter value codec for RedWire `QueryWithParams` frames.
///
/// Mirrors the engine-side Value tag table. Each parameter is encoded as one
/// tag byte followed by either a fixed-width scalar or a little-endian length.
class ValueCodec {
  static const int tagNull = 0x00;
  static const int tagBool = 0x01;
  static const int tagInt = 0x02;
  static const int tagFloat = 0x03;
  static const int tagText = 0x04;
  static const int tagBytes = 0x05;
  static const int tagVector = 0x06;
  static const int tagJson = 0x07;
  static const int tagTimestamp = 0x08;
  static const int tagUuid = 0x09;

  static const int maxParamCount = 65536;
  static const int maxValuePayloadLen = MAX_FRAME_SIZE;

  static Uint8List encodeQueryWithParams(String sql, List<Object?> params) {
    if (params.length > maxParamCount) {
      throw ArgumentError.value(
        params.length,
        'params.length',
        'must be <= $maxParamCount',
      );
    }
    final sqlBytes = utf8.encode(sql);
    if (sqlBytes.length > maxValuePayloadLen) {
      throw ArgumentError.value(
        sqlBytes.length,
        'sql.length',
        'must be <= $maxValuePayloadLen bytes',
      );
    }

    final out = BytesBuilder(copy: false);
    out.add(_u32(sqlBytes.length));
    out.add(sqlBytes);
    out.add(_u32(params.length));
    for (var i = 0; i < params.length; i++) {
      try {
        out.add(encodeValue(params[i]));
      } catch (e) {
        throw ArgumentError('param[$i]: $e');
      }
    }
    return out.toBytes();
  }

  static Uint8List encodeValue(Object? value) {
    if (value is Value) {
      return _encodeWrapped(value);
    }
    if (value == null) {
      return Uint8List.fromList([tagNull]);
    }
    if (value is bool) {
      return Uint8List.fromList([tagBool, value ? 1 : 0]);
    }
    if (value is int) {
      return _encodeI64(value, tagInt);
    }
    if (value is double) {
      return _encodeF64(value);
    }
    if (value is String) {
      return _encodeLenPrefixed(tagText, Uint8List.fromList(utf8.encode(value)));
    }
    if (value is Uint8List) {
      return _encodeLenPrefixed(tagBytes, value);
    }
    if (value is Float32List) {
      return _encodeVector(value);
    }
    if (value is List<double>) {
      return _encodeVector(Float32List.fromList(value));
    }
    if (value is DateTime) {
      final seconds = value.toUtc().millisecondsSinceEpoch ~/ 1000;
      return _encodeI64(seconds, tagTimestamp);
    }
    if (value is Map || value is List) {
      final jsonBytes = utf8.encode(_canonicalJson(value));
      return _encodeLenPrefixed(tagJson, Uint8List.fromList(jsonBytes));
    }
    throw ArgumentError('unsupported param type ${value.runtimeType}');
  }

  static List<Object?> toHttpParams(List<Object?> params) {
    return [
      for (var i = 0; i < params.length; i++) _toHttpParam(params[i], i),
    ];
  }

  static Uint8List _encodeWrapped(Value value) {
    switch (value.kind) {
      case Value.kindBytes:
        final bytes = value.value;
        if (bytes is Uint8List) {
          return _encodeLenPrefixed(tagBytes, bytes);
        }
        if (bytes is List<int>) {
          return _encodeLenPrefixed(tagBytes, Uint8List.fromList(bytes));
        }
        throw ArgumentError('bytes wrapper requires List<int>');
      case Value.kindJson:
        final jsonBytes = utf8.encode(_canonicalJson(value.value));
        return _encodeLenPrefixed(tagJson, Uint8List.fromList(jsonBytes));
      case Value.kindUuid:
        final uuid = value.value;
        if (uuid is! String) {
          throw ArgumentError('uuid wrapper requires String');
        }
        return Uint8List.fromList([tagUuid, ..._parseUuid(uuid)]);
      default:
        throw ArgumentError("unknown Value wrapper '${value.kind}'");
    }
  }

  static Object? _toHttpParam(Object? value, int index) {
    try {
      return _toHttpParamInner(value);
    } catch (e) {
      throw ArgumentError('param[$index]: $e');
    }
  }

  static Object? _toHttpParamInner(Object? value) {
    if (value is Value) {
      switch (value.kind) {
        case Value.kindBytes:
          final bytes = value.value;
          if (bytes is Uint8List) {
            return {'\$bytes': base64Encode(bytes)};
          }
          if (bytes is List<int>) {
            return {'\$bytes': base64Encode(bytes)};
          }
          throw ArgumentError('bytes wrapper requires List<int>');
        case Value.kindJson:
          return _canonicalize(value.value);
        case Value.kindUuid:
          final uuid = value.value;
          if (uuid is! String) {
            throw ArgumentError('uuid wrapper requires String');
          }
          return {'\$uuid': _formatUuid(_parseUuid(uuid))};
        default:
          throw ArgumentError("unknown Value wrapper '${value.kind}'");
      }
    }
    if (value is DateTime) {
      return {'\$ts': value.toUtc().millisecondsSinceEpoch ~/ 1000};
    }
    if (value is Uint8List) {
      return {'\$bytes': base64Encode(value)};
    }
    if (value is Float32List) {
      return [for (final v in value) v.toDouble()];
    }
    if (value is List<double>) {
      return value;
    }
    if (value is Map || value is List) {
      return _canonicalize(value);
    }
    if (value == null ||
        value is bool ||
        value is int ||
        value is double ||
        value is String) {
      return value;
    }
    throw ArgumentError('unsupported param type ${value.runtimeType}');
  }

  static Uint8List _encodeI64(int value, int tag) {
    if (value < -0x8000000000000000 || value > 0x7fffffffffffffff) {
      throw ArgumentError.value(value, 'value', 'outside signed i64 range');
    }
    final out = Uint8List(9);
    out[0] = tag;
    ByteData.sublistView(out, 1).setInt64(0, value, Endian.little);
    return out;
  }

  static Uint8List _encodeF64(double value) {
    final out = Uint8List(9);
    out[0] = tagFloat;
    ByteData.sublistView(out, 1).setFloat64(0, value, Endian.little);
    return out;
  }

  static Uint8List _encodeLenPrefixed(int tag, Uint8List bytes) {
    if (bytes.length > maxValuePayloadLen) {
      throw ArgumentError.value(
        bytes.length,
        'bytes.length',
        'must be <= $maxValuePayloadLen',
      );
    }
    final out = Uint8List(1 + 4 + bytes.length);
    out[0] = tag;
    ByteData.sublistView(out, 1).setUint32(0, bytes.length, Endian.little);
    out.setRange(5, out.length, bytes);
    return out;
  }

  static Uint8List _encodeVector(Float32List values) {
    final bytes = values.length * 4;
    if (bytes > maxValuePayloadLen) {
      throw ArgumentError.value(
        bytes,
        'vector bytes',
        'must be <= $maxValuePayloadLen',
      );
    }
    final out = Uint8List(1 + 4 + bytes);
    out[0] = tagVector;
    final data = ByteData.sublistView(out);
    data.setUint32(1, values.length, Endian.little);
    for (var i = 0; i < values.length; i++) {
      data.setFloat32(5 + i * 4, values[i], Endian.little);
    }
    return out;
  }

  static Uint8List _u32(int value) {
    final out = Uint8List(4);
    ByteData.sublistView(out).setUint32(0, value, Endian.little);
    return out;
  }

  static String _canonicalJson(Object? value) {
    return jsonEncode(_canonicalize(value));
  }

  static Object? _canonicalize(Object? value) {
    if (value is Value) {
      return _toHttpParamInner(value);
    }
    if (value is DateTime) {
      return {'\$ts': value.toUtc().millisecondsSinceEpoch ~/ 1000};
    }
    if (value is Uint8List) {
      return {'\$bytes': base64Encode(value)};
    }
    if (value is Float32List) {
      return [for (final v in value) v.toDouble()];
    }
    if (value is Map) {
      final entries = <MapEntry<String, Object?>>[];
      for (final entry in value.entries) {
        final key = entry.key;
        if (key is! String && key is! int) {
          throw ArgumentError('JSON object keys must be strings or integers');
        }
        entries.add(MapEntry(key.toString(), entry.value));
      }
      entries.sort((a, b) => a.key.compareTo(b.key));
      final out = LinkedHashMap<String, Object?>();
      for (final entry in entries) {
        out[entry.key] = _canonicalize(entry.value);
      }
      return out;
    }
    if (value is List) {
      return [for (final item in value) _canonicalize(item)];
    }
    if (value == null ||
        value is bool ||
        value is int ||
        value is double ||
        value is String) {
      return value;
    }
    throw ArgumentError('unsupported JSON param type ${value.runtimeType}');
  }

  static Uint8List _parseUuid(String uuid) {
    final hex = uuid.replaceAll('-', '').toLowerCase();
    final valid = RegExp(r'^[0-9a-f]{32}$').hasMatch(hex);
    if (!valid) {
      throw ArgumentError.value(uuid, 'uuid', 'invalid UUID');
    }
    final out = Uint8List(16);
    for (var i = 0; i < 16; i++) {
      out[i] = int.parse(hex.substring(i * 2, i * 2 + 2), radix: 16);
    }
    return out;
  }

  static String _formatUuid(Uint8List bytes) {
    final hex = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
    return '${hex.substring(0, 8)}-'
        '${hex.substring(8, 12)}-'
        '${hex.substring(12, 16)}-'
        '${hex.substring(16, 20)}-'
        '${hex.substring(20, 32)}';
  }
}
