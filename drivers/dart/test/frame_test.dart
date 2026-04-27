import 'dart:convert';
import 'dart:typed_data';

import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

void main() {
  group('frame round-trip', () {
    test('encode + decode plain Query frame', () {
      final f = Frame(
        kind: MessageKind.query,
        correlationId: 42,
        payload: Uint8List.fromList(utf8.encode('SELECT 1')),
      );
      final wire = encodeFrame(f);
      // header + payload = 16 + 8 = 24
      expect(wire.length, FRAME_HEADER_SIZE + 'SELECT 1'.length);
      final decoded = decodeFrame(wire);
      expect(decoded, isNotNull);
      expect(decoded!.consumed, wire.length);
      expect(decoded.frame.kind, MessageKind.query);
      expect(decoded.frame.correlationId, 42);
      expect(utf8.decode(decoded.frame.payload), 'SELECT 1');
    });

    test('decode returns null on truncated header', () {
      final wire = encodeFrame(Frame(
        kind: MessageKind.ping,
        correlationId: 1,
        payload: Uint8List(0),
      ));
      final partial = Uint8List.sublistView(wire, 0, 8);
      expect(decodeFrame(partial), isNull);
    });

    test('decode returns null on truncated payload', () {
      final wire = encodeFrame(Frame(
        kind: MessageKind.query,
        correlationId: 1,
        payload: Uint8List.fromList([1, 2, 3, 4, 5, 6, 7, 8]),
      ));
      // chop off last 4 bytes of payload
      final partial = Uint8List.sublistView(wire, 0, wire.length - 4);
      expect(decodeFrame(partial), isNull);
    });

    test('rejects unknown flag bits', () {
      // Hand-craft a frame with flags=0x80 (no known bit).
      final buf = Uint8List(FRAME_HEADER_SIZE);
      final view = ByteData.sublistView(buf);
      view.setUint32(0, FRAME_HEADER_SIZE, Endian.little);
      buf[4] = MessageKind.query;
      buf[5] = 0x80;
      view.setUint16(6, 0, Endian.little);
      view.setUint64(8, 1, Endian.little);
      expect(() => decodeFrame(buf), throwsA(isA<UnknownFlags>()));
    });

    test('rejects too-large encode', () {
      final huge = Uint8List(MAX_FRAME_SIZE); // header + this overflows
      final f = Frame(
        kind: MessageKind.query,
        correlationId: 1,
        payload: huge,
      );
      expect(() => encodeFrame(f), throwsA(isA<FrameTooLarge>()));
    });

    test('rejects too-large length on decode', () {
      final buf = Uint8List(FRAME_HEADER_SIZE);
      final view = ByteData.sublistView(buf);
      // length = MAX_FRAME_SIZE + 1
      view.setUint32(0, MAX_FRAME_SIZE + 1, Endian.little);
      buf[4] = MessageKind.query;
      buf[5] = 0;
      expect(() => decodeFrame(buf), throwsA(isA<ProtocolError>()));
    });

    test('header layout is little-endian as documented', () {
      final f = Frame(
        kind: MessageKind.helloAck,
        correlationId: 0x0102030405060708,
        streamId: 0xABCD,
        payload: Uint8List.fromList([0xDE, 0xAD]),
      );
      final wire = encodeFrame(f);
      // length
      expect(wire[0], FRAME_HEADER_SIZE + 2);
      expect(wire[1], 0);
      expect(wire[2], 0);
      expect(wire[3], 0);
      expect(wire[4], MessageKind.helloAck);
      expect(wire[5], 0);
      expect(wire[6], 0xCD);
      expect(wire[7], 0xAB);
      expect(wire[8], 0x08);
      expect(wire[9], 0x07);
      expect(wire[10], 0x06);
      expect(wire[11], 0x05);
      expect(wire[12], 0x04);
      expect(wire[13], 0x03);
      expect(wire[14], 0x02);
      expect(wire[15], 0x01);
    });

    test('compressed flag without zstd → falls back to plaintext on encode', () {
      final f = Frame(
        kind: MessageKind.query,
        correlationId: 1,
        flags: Flags.compressed,
        payload: Uint8List.fromList(utf8.encode('hello')),
      );
      final wire = encodeFrame(f);
      // Flag should have been cleared since no codec is registered.
      expect(wire[5], 0);
      final decoded = decodeFrame(wire)!;
      expect(utf8.decode(decoded.frame.payload), 'hello');
    });

    test('compressed flag without zstd → throws on decode', () {
      // Build a frame manually with flag set.
      final payload = Uint8List.fromList(utf8.encode('x'));
      final length = FRAME_HEADER_SIZE + payload.length;
      final buf = Uint8List(length);
      final view = ByteData.sublistView(buf);
      view.setUint32(0, length, Endian.little);
      buf[4] = MessageKind.result;
      buf[5] = Flags.compressed;
      view.setUint16(6, 0, Endian.little);
      view.setUint64(8, 1, Endian.little);
      buf.setRange(FRAME_HEADER_SIZE, length, payload);
      expect(() => decodeFrame(buf), throwsA(isA<CompressedButNoZstd>()));
    });

    test('FrameHeader.fromBytes parses fields', () {
      final wire = encodeFrame(Frame(
        kind: MessageKind.bye,
        correlationId: 7,
        payload: Uint8List(0),
      ));
      final header = FrameHeader.fromBytes(
        Uint8List.sublistView(wire, 0, FRAME_HEADER_SIZE),
      );
      expect(header.kind, MessageKind.bye);
      expect(header.correlationId, 7);
      expect(header.length, FRAME_HEADER_SIZE);
    });
  });
}
