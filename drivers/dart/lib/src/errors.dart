/// Top-level exception type for everything the driver throws.
///
/// Mirrors `RedDBError` from the JS / Python drivers. Code values stay
/// stable across drivers so users can switch on them.
class RedDBError implements Exception {
  RedDBError(this.code, this.message, [this.details]);

  final String code;
  final String message;
  final Object? details;

  @override
  String toString() => 'RedDBError($code): $message';
}

/// Server refused the handshake (bad token / bearer required / etc).
class AuthRefused extends RedDBError {
  AuthRefused(String message, [Object? details])
      : super('AUTH_REFUSED', message, details);
}

/// Wire-level protocol violation. Frame shape, missing fields, etc.
class ProtocolError extends RedDBError {
  ProtocolError(String message, [Object? details])
      : super('PROTOCOL', message, details);
}

/// Server returned a Result/Error frame indicating engine failure.
class EngineError extends RedDBError {
  EngineError(String message, [Object? details])
      : super('ENGINE', message, details);
}

/// Caller tried to send a frame larger than 16 MiB.
class FrameTooLarge extends RedDBError {
  FrameTooLarge(int size, int max)
      : super('FRAME_TOO_LARGE', 'frame $size > $max');
}

/// Frame had bits set in `flags` outside `KNOWN_FLAGS`.
class UnknownFlags extends RedDBError {
  UnknownFlags(int flags)
      : super('FRAME_UNKNOWN_FLAGS', 'flags=0x${flags.toRadixString(16)}');
}

/// Peer set the COMPRESSED flag and we don't have zstd available.
class CompressedButNoZstd extends RedDBError {
  CompressedButNoZstd()
      : super(
          'COMPRESSED_BUT_NO_ZSTD',
          'incoming frame has COMPRESSED flag but pure-Dart driver '
              "can't decompress zstd. Disable redwire compression server-side.",
        );
}

/// Caller used a `red://` URI that points at the embedded engine.
class EmbeddedUnsupported extends RedDBError {
  EmbeddedUnsupported()
      : super(
          'EMBEDDED_UNSUPPORTED',
          'embedded engine is Rust-only; use red:// or reds:// or http(s)://',
        );
}

/// Connection-string parse failure.
class InvalidUri extends RedDBError {
  InvalidUri(String message) : super('INVALID_URI', message);
}

/// Unknown URI scheme (mongodb://, etc).
class UnsupportedScheme extends RedDBError {
  UnsupportedScheme(String message) : super('UNSUPPORTED_SCHEME', message);
}
