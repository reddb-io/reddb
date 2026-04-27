import 'dart:typed_data';

/// Optional zstd codec. Pure-Dart drivers don't ship a zstd
/// implementation; this is a hook for users who want to wire one in
/// (e.g. via `package:zstandard` or FFI on the server side).
///
/// The frame codec calls [encode] when the COMPRESSED flag is set on
/// outbound frames, and [decode] when it sees the bit on inbound. If
/// either returns `null` the frame falls back to plaintext (encode) or
/// raises `CompressedButNoZstd` (decode).
abstract class ZstdCodec {
  /// Compress `data`. Return `null` to signal the caller should ship
  /// plaintext.
  Uint8List? encode(Uint8List data);

  /// Decompress `data`. Return `null` if the bytes can't be decoded —
  /// the frame decoder will then raise `CompressedButNoZstd`.
  Uint8List? decode(Uint8List data);
}
