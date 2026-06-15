import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:reddb/reddb.dart';
import 'package:reddb/src/redwire/value_codec.dart';
import 'package:test/test.dart';

void main() {
  group('ValueCodec', () {
    test('value tag table is pinned', () {
      expect(ValueCodec.tagNull, 0x00);
      expect(ValueCodec.tagBool, 0x01);
      expect(ValueCodec.tagInt, 0x02);
      expect(ValueCodec.tagFloat, 0x03);
      expect(ValueCodec.tagText, 0x04);
      expect(ValueCodec.tagBytes, 0x05);
      expect(ValueCodec.tagVector, 0x06);
      expect(ValueCodec.tagJson, 0x07);
      expect(ValueCodec.tagTimestamp, 0x08);
      expect(ValueCodec.tagUuid, 0x09);
    });

    test('encodes scalar values', () {
      expect(ValueCodec.encodeValue(null), equals(<int>[0x00]));
      expect(ValueCodec.encodeValue(true), equals(<int>[0x01, 0x01]));
      expect(ValueCodec.encodeValue(false), equals(<int>[0x01, 0x00]));
      expect(
        ValueCodec.encodeValue(1),
        equals(<int>[0x02, 0x01, 0, 0, 0, 0, 0, 0, 0]),
      );
      expect(
        ValueCodec.encodeValue(-1),
        equals(<int>[0x02, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
      );
      expect(
        ValueCodec.encodeValue('x'),
        equals(<int>[0x04, 0x01, 0, 0, 0, 0x78]),
      );
    });

    test('encodes bytes timestamp uuid and json', () {
      expect(
        ValueCodec.encodeValue(Uint8List.fromList([0xde, 0xad, 0xbe, 0xef])),
        equals(<int>[0x05, 0x04, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef]),
      );

      final ts = ValueCodec.encodeValue(
        DateTime.fromMillisecondsSinceEpoch(1700000000123, isUtc: true),
      );
      expect(ts[0], ValueCodec.tagTimestamp);
      expect(
        ByteData.sublistView(ts, 1).getInt64(0, Endian.little),
        1700000000,
      );

      final uuid = ValueCodec.encodeValue(
        Value.uuid('00112233-4455-6677-8899-aabbccddeeff'),
      );
      expect(uuid[0], ValueCodec.tagUuid);
      expect(
        Uint8List.sublistView(uuid, 1),
        equals(<int>[
          0x00,
          0x11,
          0x22,
          0x33,
          0x44,
          0x55,
          0x66,
          0x77,
          0x88,
          0x99,
          0xaa,
          0xbb,
          0xcc,
          0xdd,
          0xee,
          0xff,
        ]),
      );

      final json = ValueCodec.encodeValue(Value.json({'b': 2, 'a': 1}));
      expect(json[0], ValueCodec.tagJson);
      expect(ByteData.sublistView(json, 1).getUint32(0, Endian.little), 13);
      expect(utf8.decode(Uint8List.sublistView(json, 5)), '{"a":1,"b":2}');
    });

    test('encodes vector from Float32List and List<double>', () {
      final encoded = ValueCodec.encodeValue(
        Float32List.fromList([1.0, 2.0, -0.5]),
      );
      expect(encoded[0], ValueCodec.tagVector);
      expect(ByteData.sublistView(encoded, 1).getUint32(0, Endian.little), 3);
      expect(
        ByteData.sublistView(encoded, 5).getFloat32(0, Endian.little),
        1.0,
      );
      expect(
        ByteData.sublistView(encoded, 9).getFloat32(0, Endian.little),
        2.0,
      );
      expect(
        ByteData.sublistView(encoded, 13).getFloat32(0, Endian.little),
        -0.5,
      );

      final fromList = ValueCodec.encodeValue(<double>[1.5, -2.5]);
      expect(fromList[0], ValueCodec.tagVector);
      expect(ByteData.sublistView(fromList, 1).getUint32(0, Endian.little), 2);
    });

    test('encodes query with params payload', () {
      final encoded = ValueCodec.encodeQueryWithParams('Q', [42, 'x', null]);
      expect(ByteData.sublistView(encoded, 0).getUint32(0, Endian.little), 1);
      expect(encoded[4], 'Q'.codeUnitAt(0));
      expect(ByteData.sublistView(encoded, 5).getUint32(0, Endian.little), 3);
      expect(encoded[9], ValueCodec.tagInt);
      expect(ByteData.sublistView(encoded, 10).getInt64(0, Endian.little), 42);
      expect(encoded[18], ValueCodec.tagText);
      expect(
        Uint8List.sublistView(encoded, 19, 24),
        equals(<int>[1, 0, 0, 0, 0x78]),
      );
      expect(encoded[24], ValueCodec.tagNull);
      expect(encoded.length, 25);
    });

    test('http params use JSON envelopes for tagged values', () {
      final params = ValueCodec.toHttpParams([
        null,
        true,
        42,
        1.5,
        'txt',
        Uint8List.fromList(utf8.encode('hi')),
        Float32List.fromList([1.0, 2.0]),
        Value.json({'b': 2, 'a': 1}),
        DateTime.fromMillisecondsSinceEpoch(1700000000123, isUtc: true),
        Value.uuid('00112233-4455-6677-8899-aabbccddeeff'),
      ]);

      expect(params, [
        null,
        true,
        42,
        1.5,
        'txt',
        {'\$bytes': 'aGk='},
        [1.0, 2.0],
        {'a': 1, 'b': 2},
        {'\$ts': 1700000000},
        {'\$uuid': '00112233-4455-6677-8899-aabbccddeeff'},
      ]);
    });

    test('shared parameter fixtures match manifest', () {
      final manifest = jsonDecode(
        File('../../testdata/conformance/redwire/params/manifest.json')
            .readAsStringSync(),
      ) as Map<String, dynamic>;

      for (final fixture in manifest['values'] as List<dynamic>) {
        final item = fixture as Map<String, dynamic>;
        expect(
          _hex(ValueCodec.encodeValue(_fixtureValue(item['name'] as String))),
          item['redwire_hex'],
          reason: item['name'] as String,
        );
      }

      final query = (manifest['queries'] as List<dynamic>).first as Map<String, dynamic>;
      final params = [
        for (final name in query['params'] as List<dynamic>) _fixtureValue(name as String),
      ];
      expect(
        _hex(ValueCodec.encodeQueryWithParams(query['sql'] as String, params)),
        query['redwire_hex'],
        reason: query['name'] as String,
      );
    });
  });
}

Object? _fixtureValue(String name) {
  switch (name) {
    case 'null':
      return null;
    case 'bool_true':
      return true;
    case 'bool_false':
      return false;
    case 'int_min':
      return -0x8000000000000000;
    case 'int_max':
      return 0x7fffffffffffffff;
    case 'int_42':
      return 42;
    case 'float_nan':
      return _doubleFromBits(0x7ff8000000000000);
    case 'float_pos_inf':
      return double.infinity;
    case 'float_neg_inf':
      return double.negativeInfinity;
    case 'float_subnormal_min':
      return _doubleFromBits(1);
    case 'text_unicode':
      return 'h\u00e9llo';
    case 'text_x':
      return 'x';
    case 'bytes_empty':
      return Uint8List(0);
    case 'bytes_deadbeef':
      return Uint8List.fromList([0xde, 0xad, 0xbe, 0xef]);
    case 'bytes_256':
      return Uint8List.fromList(List<int>.generate(256, (i) => i));
    case 'json_nested':
      return Value.json({
        'z': [1, {'deep': [true, false]}],
        'a': null,
      });
    case 'timestamp_zero':
      return Value.timestamp(0);
    case 'timestamp_max':
      return Value.timestamp(0x7fffffffffffffff);
    case 'uuid_001122':
      return Value.uuid('00112233-4455-6677-8899-aabbccddeeff');
    case 'vector_empty':
      return Float32List(0);
    case 'vector_three':
      return Float32List.fromList([1.0, 2.0, -0.5]);
    case 'vector_128':
      return Float32List.fromList(
        List<double>.generate(128, (i) => i.toDouble()),
      );
    default:
      throw ArgumentError('unknown fixture $name');
  }
}

double _doubleFromBits(int bits) {
  final bytes = ByteData(8)..setUint64(0, bits, Endian.host);
  return bytes.getFloat64(0, Endian.host);
}

String _hex(Uint8List bytes) =>
    bytes.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();
