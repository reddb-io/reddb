import 'dart:typed_data';

import '../errors.dart';
import 'codec.dart' show ZstdCodec;

/// Magic byte that identifies a RedWire connection on the shared
/// listener.
const int MAGIC = 0xFE;

/// Highest minor protocol version this driver implements.
const int SUPPORTED_VERSION = 0x01;

/// Fixed header size in bytes.
const int FRAME_HEADER_SIZE = 16;

/// Hard cap — anything above gets rejected on encode and decode.
const int MAX_FRAME_SIZE = 16 * 1024 * 1024;

/// Bits that are valid in the `flags` field. Anything else is a
/// protocol error.
const int KNOWN_FLAGS = 0x03;

/// Frame kind constants. Match `wire::redwire::frame::MessageKind`.
class MessageKind {
  static const int query = 0x01;
  static const int result = 0x02;
  static const int error = 0x03;
  static const int bulkInsert = 0x04;
  static const int bulkOk = 0x05;
  static const int bulkInsertBinary = 0x06;
  static const int queryBinary = 0x07;
  static const int bulkInsertPrevalidated = 0x08;
  static const int hello = 0x10;
  static const int helloAck = 0x11;
  static const int authRequest = 0x12;
  static const int authResponse = 0x13;
  static const int authOk = 0x14;
  static const int authFail = 0x15;
  static const int bye = 0x16;
  static const int ping = 0x17;
  static const int pong = 0x18;
  static const int get = 0x19;
  static const int delete = 0x1A;
  static const int deleteOk = 0x1B;

  /// Reverse map for human-readable error messages.
  static String name(int kind) {
    switch (kind) {
      case query:
        return 'Query';
      case result:
        return 'Result';
      case error:
        return 'Error';
      case bulkInsert:
        return 'BulkInsert';
      case bulkOk:
        return 'BulkOk';
      case bulkInsertBinary:
        return 'BulkInsertBinary';
      case queryBinary:
        return 'QueryBinary';
      case bulkInsertPrevalidated:
        return 'BulkInsertPrevalidated';
      case hello:
        return 'Hello';
      case helloAck:
        return 'HelloAck';
      case authRequest:
        return 'AuthRequest';
      case authResponse:
        return 'AuthResponse';
      case authOk:
        return 'AuthOk';
      case authFail:
        return 'AuthFail';
      case bye:
        return 'Bye';
      case ping:
        return 'Ping';
      case pong:
        return 'Pong';
      case get:
        return 'Get';
      case delete:
        return 'Delete';
      case deleteOk:
        return 'DeleteOk';
      default:
        return '0x${kind.toRadixString(16).padLeft(2, '0')}';
    }
  }
}

/// Bit values for the frame `flags` byte.
class Flags {
  static const int compressed = 0x01;
  static const int moreFrames = 0x02;
}

/// Decoded view of a frame's fixed header.
class FrameHeader {
  const FrameHeader({
    required this.length,
    required this.kind,
    required this.flags,
    required this.streamId,
    required this.correlationId,
  });

  /// Total frame length (header + payload), little-endian u32.
  final int length;
  final int kind;
  final int flags;
  final int streamId;
  final int correlationId;

  /// Parse a 16-byte buffer into a header. Caller must pass exactly
  /// [FRAME_HEADER_SIZE] bytes.
  factory FrameHeader.fromBytes(Uint8List header) {
    if (header.length < FRAME_HEADER_SIZE) {
      throw ProtocolError('frame header truncated: ${header.length} bytes');
    }
    final view = ByteData.sublistView(header);
    final length = view.getUint32(0, Endian.little);
    if (length < FRAME_HEADER_SIZE || length > MAX_FRAME_SIZE) {
      throw ProtocolError('frame length invalid: $length');
    }
    return FrameHeader(
      length: length,
      kind: header[4],
      flags: header[5],
      streamId: view.getUint16(6, Endian.little),
      correlationId: view.getUint64(8, Endian.little),
    );
  }
}

/// Decoded RedWire frame.
class Frame {
  Frame({
    required this.kind,
    required this.correlationId,
    required this.payload,
    this.flags = 0,
    this.streamId = 0,
  });

  final int kind;
  final int flags;
  final int streamId;
  final int correlationId;
  final Uint8List payload;
}

/// Encode `frame` to bytes.
///
/// When [Flags.compressed] is set on the input frame and the optional
/// [zstd] codec accepts the input, the payload is compressed and the
/// flag is preserved. Otherwise the payload is shipped plain and the
/// flag is cleared (server still accepts plaintext frames regardless).
Uint8List encodeFrame(Frame frame, {ZstdCodec? zstd}) {
  Uint8List onWire = frame.payload;
  int outFlags = frame.flags & KNOWN_FLAGS;
  if ((outFlags & Flags.compressed) != 0) {
    final compressed = zstd?.encode(frame.payload);
    if (compressed != null) {
      onWire = compressed;
    } else {
      // Pure-Dart fallback: no zstd available, ship plain.
      outFlags &= ~Flags.compressed;
    }
  }
  final length = FRAME_HEADER_SIZE + onWire.length;
  if (length > MAX_FRAME_SIZE) {
    throw FrameTooLarge(length, MAX_FRAME_SIZE);
  }
  final buf = Uint8List(length);
  final view = ByteData.sublistView(buf);
  view.setUint32(0, length, Endian.little);
  buf[4] = frame.kind & 0xFF;
  buf[5] = outFlags & 0xFF;
  view.setUint16(6, frame.streamId & 0xFFFF, Endian.little);
  view.setUint64(8, frame.correlationId, Endian.little);
  buf.setRange(FRAME_HEADER_SIZE, length, onWire);
  return buf;
}

/// Try to decode a frame from `bytes`. Returns `null` when more bytes
/// are needed (incomplete frame) — caller buffers and retries.
///
/// Throws [ProtocolError] / [UnknownFlags] / [CompressedButNoZstd] on
/// malformed input.
({Frame frame, int consumed})? decodeFrame(
  Uint8List bytes, {
  ZstdCodec? zstd,
}) {
  if (bytes.length < FRAME_HEADER_SIZE) return null;
  final header = FrameHeader.fromBytes(
    Uint8List.sublistView(bytes, 0, FRAME_HEADER_SIZE),
  );
  if (bytes.length < header.length) return null;

  if ((header.flags & ~KNOWN_FLAGS) != 0) {
    throw UnknownFlags(header.flags);
  }

  Uint8List payload = Uint8List.sublistView(
    bytes,
    FRAME_HEADER_SIZE,
    header.length,
  );

  if ((header.flags & Flags.compressed) != 0) {
    final plain = zstd?.decode(payload);
    if (plain == null) {
      throw CompressedButNoZstd();
    }
    payload = plain;
  } else {
    // Detach from the underlying buffer so callers can reuse it.
    payload = Uint8List.fromList(payload);
  }

  return (
    frame: Frame(
      kind: header.kind,
      flags: header.flags,
      streamId: header.streamId,
      correlationId: header.correlationId,
      payload: payload,
    ),
    consumed: header.length,
  );
}
