import 'dart:convert';
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
  });
}
